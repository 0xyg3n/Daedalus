//! Verified native workspace operations. UI dialogs never construct shell commands.
use super::session_actions::{self, AttachContext, AttachMode};
use crate::adapters::terminals::{self, Terminals};
use crate::core::identity::Backend;
use serde_json::Value;
use std::{
    collections::BTreeMap,
    fs::OpenOptions,
    process::{Command, Stdio},
    time::{SystemTime, UNIX_EPOCH},
};
type Result<T> = std::result::Result<T, String>;
fn text<'a>(v: &'a Value, key: &str) -> Result<&'a str> {
    v[key]
        .as_str()
        .filter(|s| !s.is_empty() && !s.chars().any(char::is_control))
        .ok_or_else(|| format!("Missing workspace {key}"))
}
/// The caller must re-run preparation after the external-ready reply. The
/// returned command uses the real TTY only after the existing renderer yields.
pub fn native_command(
    adapter: &Terminals,
    pane: &Value,
    snapshot: &Value,
    context: &AttachContext,
    environment: &BTreeMap<String, String>,
) -> Result<Command> {
    session_actions::prepare_attach(adapter, pane, AttachMode::Control, snapshot, context)?;
    let actual = adapter.verify(pane)?;
    let id = terminals::identity(&actual)?;
    let mut command = if id.backend == Backend::Native {
        let mut command =
            Command::new(std::env::current_exe().map_err(|_| "Cannot locate Daedalus executable")?);
        command.args(["terminal-native", "attach", &id.endpoint, &id.generation]);
        command
    } else if id.backend == Backend::Tmux {
        let name = format!(
            "daedalus-view-{:x}-{:x}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_err(|_| "Invalid system time")?
                .as_nanos()
        );
        let index = text(&actual, "window_index")?;
        if !index.bytes().all(|b| b.is_ascii_digit()) {
            return Err("Invalid tmux window index".into());
        }
        let mut command = Command::new(&adapter.tmux);
        command.args([
            "-S",
            &id.endpoint,
            "new-session",
            "-d",
            "-t",
            text(&actual, "session_id")?,
            "-s",
            &name,
            ";",
            "select-window",
            "-t",
            &format!("{name}:{index}"),
            ";",
            "set-option",
            "-t",
            &name,
            "destroy-unattached",
            "on",
            ";",
            "attach-session",
            "-t",
            &name,
        ]);
        command
    } else {
        let mut command = Command::new(&adapter.herdr);
        command.args([
            "--session",
            &id.endpoint,
            "terminal",
            "attach",
            &id.generation,
        ]);
        command
    };
    command.env_clear();
    for (key, value) in environment {
        if [
            "HOME",
            "PATH",
            "TERM",
            "COLORTERM",
            "LANG",
            "LANGUAGE",
            "TZ",
            "XDG_RUNTIME_DIR",
            "USER",
            "LOGNAME",
        ]
        .contains(&key.as_str())
            || key.starts_with("LC_")
        {
            command.env(key, value);
        }
    }
    Ok(command)
}
pub fn run_native(mut command: Command) -> Result<String> {
    let tty = OpenOptions::new()
        .read(true)
        .write(true)
        .open("/dev/tty")
        .map_err(|_| "Native client needs a controlling terminal")?;
    command
        .stdin(Stdio::from(
            tty.try_clone().map_err(|_| "Cannot attach native stdin")?,
        ))
        .stdout(Stdio::from(
            tty.try_clone().map_err(|_| "Cannot attach native stdout")?,
        ))
        .stderr(Stdio::from(tty));
    let status = command
        .status()
        .map_err(|_| "Native client outcome is unconfirmed; inspect the server before retrying")?;
    if !status.success() {
        return Err(
            "Native client exited unsuccessfully; the original terminal was not closed".into(),
        );
    }
    Ok("Native client detached; original terminal remains running".into())
}

use super::config::ControlConfig;
use crate::{
    adapters::{
        harnesses,
        terminal_actions::{Placement, Receipt, ReceiptState, validate_launch},
    },
    core::checkpoints::{LaunchPlan, Operation},
};
use std::path::Path;
fn receipt(state: ReceiptState, message: &str, terminal: Option<Value>) -> Receipt {
    Receipt {
        state,
        message: message.into(),
        terminal,
    }
}
pub fn launch_plan(
    tool: &str,
    cwd: &Path,
    config: &ControlConfig,
    environment: &BTreeMap<String, String>,
) -> Result<LaunchPlan> {
    let (binary, args, environment) = if tool == "shell" {
        let environment: BTreeMap<String, String> = environment
            .iter()
            .filter(|(key, _)| {
                [
                    "HOME",
                    "PATH",
                    "TERM",
                    "COLORTERM",
                    "LANG",
                    "LANGUAGE",
                    "TZ",
                    "USER",
                    "LOGNAME",
                    "XDG_RUNTIME_DIR",
                    "SystemRoot",
                    "SYSTEMROOT",
                    "WINDIR",
                    "USERPROFILE",
                    "APPDATA",
                    "LOCALAPPDATA",
                    "HOMEDRIVE",
                    "HOMEPATH",
                    "TEMP",
                    "TMP",
                    "PATHEXT",
                    "COMSPEC",
                ]
                .contains(&key.as_str())
                    || key.starts_with("LC_")
            })
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        #[cfg(unix)]
        let shell = ("/bin/sh".into(), vec!["-i".into()], environment);
        #[cfg(windows)]
        let shell = (
            windows_shell(&environment)?,
            vec!["-NoLogo".into(), "-NoProfile".into()],
            environment,
        );
        shell
    } else {
        let adapter = harnesses::tool(tool).ok_or("Unknown harness")?;
        let executable = harnesses::executable(&adapter, environment)
            .ok_or("Install this harness before creating a terminal")?;
        let plan =
            harnesses::launch_plan(&adapter, &executable, cwd, environment, &config.profiles)?;
        (plan.binary, plan.args, plan.environment)
    };
    // Operation is not interpreted by terminal transports. Fresh launches carry
    // no checkpoint, resume flag, session ID, or inherited conversation.
    let plan = LaunchPlan {
        binary,
        args,
        environment,
        cwd: cwd.into(),
        operation: Operation::Handoff,
    };
    validate_launch(&plan)?;
    Ok(plan)
}
fn label(value: &str) -> Result<()> {
    if value.trim().is_empty() || value.chars().count() > 80 || value.chars().any(char::is_control)
    {
        Err("Use a printable label of 1 to 80 characters".into())
    } else {
        Ok(())
    }
}
fn exact_group(adapter: &Terminals, pane: &Value) -> Result<Value> {
    let actual = adapter.verify(pane)?;
    let keys = if pane["backend"] == "tmux" {
        ["session_id", "window_id"]
    } else {
        ["workspace_id", "tab_id"]
    };
    for key in keys {
        if text(pane, key)? != text(&actual, key)? {
            return Err("Terminal grouping changed; refresh before organizing it".into());
        }
    }
    Ok(actual)
}
fn created_herdr(
    adapter: &Terminals,
    server: &str,
    response: &Value,
    launch: &LaunchPlan,
) -> Result<Receipt> {
    let Some(mut pane) = response
        .get("root_pane")
        .or_else(|| response.get("pane"))
        .and_then(Value::as_object)
        .cloned()
    else {
        return Ok(receipt(
            ReceiptState::Unconfirmed,
            "Workspace created but its terminal identity is unconfirmed; inspect before retrying",
            None,
        ));
    };
    pane.insert("backend".into(), "herdr".into());
    pane.insert("server".into(), server.into());
    pane.insert("session".into(), server.into());
    let pane = Value::Object(pane);
    if terminals::identity(&pane).is_err() {
        return Ok(receipt(
            ReceiptState::Unconfirmed,
            "Created terminal did not include its exact generation; no tool was started",
            None,
        ));
    }
    adapter.launch_created_herdr(&pane, launch)
}
pub fn create(
    adapter: &Terminals,
    backend: Backend,
    name: &str,
    anchor: Option<&Value>,
    launch: &LaunchPlan,
) -> Result<Receipt> {
    if name.is_empty()
        || name.len() > 80
        || !name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_-".contains(&b))
    {
        return Err("Use letters, digits, hyphens and underscores for the project name".into());
    }
    validate_launch(launch)?;
    let actual = anchor.map(|p| adapter.verify(p)).transpose()?;
    if actual
        .as_ref()
        .is_some_and(|p| terminals::identity(p).is_ok_and(|id| id.backend != backend))
    {
        return Err("Project backend does not match its selected server".into());
    }
    if backend == Backend::Native {
        let pane = crate::adapters::native::start(launch, name, name)?;
        return Ok(receipt(
            ReceiptState::Accepted,
            "Native workspace and terminal started",
            Some(pane),
        ));
    }
    if backend == Backend::Herdr {
        let actual = actual.as_ref().ok_or("Choose an existing Herdr server")?;
        let server = text(actual, "session")?;
        let result = adapter.herdr_result(
            server,
            &[
                "workspace",
                "create",
                "--cwd",
                launch.cwd.to_str().ok_or("Project path is not UTF-8")?,
                "--label",
                name,
                "--no-focus",
            ],
        );
        return match result {
            Ok(result) => created_herdr(adapter, server, &result, launch),
            Err(_) => Ok(receipt(
                ReceiptState::Unconfirmed,
                "Workspace creation is unconfirmed; inspect before retrying",
                None,
            )),
        };
    }
    let mut args = Vec::<String>::new();
    if let Some(actual) = &actual {
        args.extend(["-S".into(), text(actual, "socket")?.into()]);
    }
    args.extend([
        "new-session".into(),
        "-d".into(),
        "-s".into(),
        name.into(),
        "-c".into(),
        launch.cwd.to_string_lossy().into_owned(),
        "-P".into(),
        "-F".into(),
        "#{socket_path}\t#{pane_id}".into(),
        "--".into(),
    ]);
    #[cfg(windows)]
    args.extend(crate::adapters::windows_launch::argv(launch)?);
    #[cfg(unix)]
    {
        args.extend(["/usr/bin/env".into(), "-i".into()]);
        for (key, value) in &launch.environment {
            args.push(format!("{key}={value}"));
        }
        args.push(launch.binary.to_string_lossy().into_owned());
        args.extend(launch.args.clone());
    }
    let mut process = Command::new(&adapter.tmux);
    process.args(&args).env_clear();
    for (key, value) in &launch.environment {
        if [
            "HOME",
            "PATH",
            "TERM",
            "LANG",
            "XDG_RUNTIME_DIR",
            "SystemRoot",
            "SYSTEMROOT",
            "WINDIR",
            "USERPROFILE",
            "APPDATA",
            "LOCALAPPDATA",
            "HOMEDRIVE",
            "HOMEPATH",
            "TEMP",
            "TMP",
            "PATHEXT",
            "COMSPEC",
        ]
        .contains(&key.as_str())
        {
            process.env(key, value);
        }
    }
    let output =
        match crate::adapters::process::run_terminal(&mut process, adapter.timeout, 8_000_000) {
            Ok(output) if output.status.success() => match String::from_utf8(output.stdout) {
                Ok(value) => value,
                Err(_) => {
                    return Ok(receipt(
                        ReceiptState::Unconfirmed,
                        "Project creation returned invalid terminal identity output",
                        None,
                    ));
                }
            },
            _ => {
                return Ok(receipt(
                    ReceiptState::Unconfirmed,
                    "Project creation is unconfirmed; inspect before retrying",
                    None,
                ));
            }
        };
    let Some((socket, pane_id)) = output.trim().split_once('\t') else {
        return Ok(receipt(
            ReceiptState::Unconfirmed,
            "Project creation returned no exact terminal identity",
            None,
        ));
    };
    let inventory = match adapter.tmux_inventory(socket) {
        Ok(i) => i,
        Err(_) => {
            return Ok(receipt(
                ReceiptState::Unconfirmed,
                "Project was requested but cannot be verified",
                None,
            ));
        }
    };
    let created = inventory["panes"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|p| p["pane_id"] == pane_id && p["session"] == name)
        .collect::<Vec<_>>();
    if created.len() != 1 || terminals::identity(created[0]).is_err() {
        return Ok(receipt(
            ReceiptState::Unconfirmed,
            "Created terminal identity is ambiguous or missing",
            None,
        ));
    }
    Ok(receipt(
        ReceiptState::Submitted,
        "Project created; harness readiness is not confirmed",
        Some(created[0].clone()),
    ))
}
pub fn organize(
    adapter: &Terminals,
    pane: &Value,
    operation: &str,
    value: &str,
    launch: Option<&LaunchPlan>,
) -> Result<Receipt> {
    let actual = exact_group(adapter, pane)?;
    let identity = terminals::identity(&actual)?;
    if ["rename-project", "rename-group", "new-group"].contains(&operation) {
        label(value)?;
    }
    if operation == "unlink-group" {
        return session_actions::unlink_window(adapter, pane);
    }
    if operation == "split" {
        return adapter.spawn(
            pane,
            launch.ok_or("Choose a tool or shell")?,
            Placement::Split,
        );
    }
    if identity.backend == Backend::Native {
        if operation == "rename-project" {
            super::native_terminal::rename(
                &identity.endpoint,
                &identity.generation,
                text(&actual, "title")?,
                value,
            )?;
            return Ok(receipt(
                ReceiptState::Accepted,
                "Terminal moved to workspace",
                None,
            ));
        }
        return Err("Use Split, Rename terminal, or Move to workspace for native terminals".into());
    }
    if operation == "new-group" {
        let launch = launch.ok_or("Choose a tool or shell")?;
        validate_launch(launch)?;
        if identity.backend == Backend::Herdr {
            return match adapter.herdr_result(
                &identity.endpoint,
                &[
                    "tab",
                    "create",
                    "--workspace",
                    text(&actual, "workspace_id")?,
                    "--label",
                    value,
                    "--no-focus",
                ],
            ) {
                Ok(result) => created_herdr(adapter, &identity.endpoint, &result, launch),
                Err(_) => Ok(receipt(
                    ReceiptState::Unconfirmed,
                    "Tab creation is unconfirmed; inspect before retrying",
                    None,
                )),
            };
        }
        let mut created = adapter.spawn(pane, launch, Placement::Window)?;
        if let Some(terminal) = &created.terminal {
            let renamed = organize(adapter, terminal, "rename-group", value, None);
            if renamed.is_err() || renamed.is_ok_and(|r| r.state == ReceiptState::Unconfirmed) {
                created.state = ReceiptState::Unconfirmed;
                created.message = "New window exists; its label change is unconfirmed".into();
            }
        }
        return Ok(created);
    }
    let result = if identity.backend == Backend::Tmux {
        let args = match operation {
            "rename-project" => vec!["rename-session", "-t", text(&actual, "session_id")?, value],
            "rename-group" => vec!["rename-window", "-t", text(&actual, "window_id")?, value],
            "close-group" => vec!["kill-window", "-t", text(&actual, "window_id")?],
            "close-project" => vec!["kill-session", "-t", text(&actual, "session_id")?],
            _ => return Err("Unsupported organization action".into()),
        };
        let mut full = vec!["-S", identity.endpoint.as_str()];
        full.extend(args);
        adapter.query(&adapter.tmux, &full).map(|_| ())
    } else {
        let args = match operation {
            "rename-project" => vec!["workspace", "rename", text(&actual, "workspace_id")?, value],
            "rename-group" => vec!["tab", "rename", text(&actual, "tab_id")?, value],
            "close-group" => vec!["tab", "close", text(&actual, "tab_id")?],
            "close-project" => vec!["workspace", "close", text(&actual, "workspace_id")?],
            _ => return Err("Unsupported organization action".into()),
        };
        adapter.herdr_result(&identity.endpoint, &args).map(|_| ())
    };
    Ok(receipt(
        if result.is_ok() {
            ReceiptState::Accepted
        } else {
            ReceiptState::Unconfirmed
        },
        if result.is_ok() {
            "Organization action accepted"
        } else {
            "Organization outcome is unconfirmed; inspect before retrying"
        },
        None,
    ))
}

#[derive(Clone, Debug)]
pub enum WorkspaceRequest {
    Create {
        backend: Backend,
        anchor: Option<Value>,
        name: String,
        cwd: String,
        tool: String,
    },
    Organize {
        pane: Value,
        operation: String,
        label: String,
        tool: String,
    },
}
impl WorkspaceRequest {
    pub fn set_tool(&mut self, selected: &str) {
        match self {
            Self::Create { tool, .. } | Self::Organize { tool, .. } => *tool = selected.into(),
        }
    }
    pub fn description(&self) -> String {
        match self {
            Self::Create {
                backend,
                name,
                cwd,
                tool,
                ..
            } => format!(
                "Create {backend:?} project {name} in {cwd}, starting {tool}. Existing projects keep running."
            ),
            Self::Organize {
                pane,
                operation,
                label,
                tool,
            } => format!(
                "{operation} on {}/{} · {label} · {tool}. Group changes affect shared views; closing a shared group closes all its terminals.",
                pane["session"].as_str().unwrap_or(""),
                pane["pane_id"].as_str().unwrap_or("")
            ),
        }
    }
    pub fn apply(
        self,
        adapter: &Terminals,
        config: &ControlConfig,
        environment: &BTreeMap<String, String>,
        directory: &Path,
    ) -> Result<Receipt> {
        match self {
            Self::Create {
                backend,
                anchor,
                name,
                cwd,
                tool,
            } => {
                let mut launch = launch_plan(&tool, Path::new(&cwd), config, environment)?;
                super::gateway::wrap(
                    directory,
                    &tool,
                    &mut launch,
                    &std::env::current_exe().map_err(|_| "Cannot locate native launcher")?,
                )?;
                create(adapter, backend, &name, anchor.as_ref(), &launch)
            }
            Self::Organize {
                pane,
                operation,
                label,
                tool,
            } => {
                let mut launch = if ["new-group", "split"].contains(&operation.as_str()) {
                    Some(launch_plan(
                        &tool,
                        Path::new(text(&pane, "cwd")?),
                        config,
                        environment,
                    )?)
                } else {
                    None
                };
                if let Some(launch) = &mut launch {
                    super::gateway::wrap(
                        directory,
                        &tool,
                        launch,
                        &std::env::current_exe().map_err(|_| "Cannot locate native launcher")?,
                    )?;
                }
                organize(adapter, &pane, &operation, &label, launch.as_ref())
            }
        }
    }
}

#[cfg(windows)]
fn windows_shell(environment: &BTreeMap<String, String>) -> Result<std::path::PathBuf> {
    let mut candidates =
        std::env::split_paths(environment.get("PATH").map(String::as_str).unwrap_or(""))
            .filter(|path| path.is_absolute())
            .flat_map(|directory| [directory.join("pwsh.exe"), directory.join("powershell.exe")])
            .collect::<Vec<_>>();
    if let Some(root) = environment
        .get("SystemRoot")
        .or_else(|| environment.get("SYSTEMROOT"))
    {
        let root = Path::new(root);
        if root.is_absolute() {
            candidates.push(root.join("System32/WindowsPowerShell/v1.0/powershell.exe"));
        }
    }
    candidates.into_iter().find(|path| crate::platform::open_regular(path).is_ok())
        .ok_or("Native Windows shell is unavailable; install PowerShell or provide its absolute PATH directory".into())
}
#[cfg(all(test, windows))]
mod windows_launch_tests {
    use super::*;
    #[test]
    fn shell_launch_preserves_required_windows_environment_without_importing_secrets() {
        let cwd = std::env::temp_dir();
        let config = ControlConfig::fresh(&cwd).unwrap();
        let mut environment = crate::platform::environment();
        environment.insert("TELEGRAM_BOT_TOKEN".into(), "private-fixture".into());
        environment.insert("UNRELATED_SECRET".into(), "private-fixture".into());
        let plan = launch_plan("shell", &cwd, &config, &environment).unwrap();
        assert!(plan.binary.is_absolute());
        assert!(plan.binary.extension().unwrap().eq_ignore_ascii_case("exe"));
        assert_eq!(plan.args, ["-NoLogo", "-NoProfile"]);
        assert!(!plan.environment.contains_key("TELEGRAM_BOT_TOKEN"));
        assert!(!plan.environment.contains_key("UNRELATED_SECRET"));
        assert!(
            plan.environment.contains_key("SystemRoot")
                || plan.environment.contains_key("SYSTEMROOT")
        );
        let command = crate::adapters::windows_launch::command(&plan).unwrap();
        assert!(command.contains("-EncodedCommand"));
        assert!(!command.contains("private-fixture"));
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn own_terminal_is_refused_before_any_native_process() {
        let pane = json!({"backend":"herdr","session":"fixture","pane_id":"pane","terminal_id":"generation"});
        let context = AttachContext {
            own_terminals: vec![terminals::identity(&pane).unwrap()],
            ..AttachContext::default()
        };
        let adapter = Terminals {
            herdr: "/does-not-exist".into(),
            ..Terminals::default()
        };
        assert!(native_command(&adapter, &pane, &json!({}), &context, &BTreeMap::new()).is_err());
    }
    #[test]
    fn herdr_native_client_plan_is_exact_and_does_not_forward_credentials() {
        use std::{fs, os::unix::fs::PermissionsExt};
        let directory =
            std::env::temp_dir().join(format!("daedalus-native-client-{}", std::process::id()));
        fs::create_dir(&directory).unwrap();
        let executable = directory.join("herdr");
        fs::write(&executable,"#!/bin/sh\nprintf '%s\\n' '{\"pane\":{\"pane_id\":\"pane\",\"terminal_id\":\"generation\"}}'\n").unwrap();
        fs::set_permissions(&executable, fs::Permissions::from_mode(0o700)).unwrap();
        let adapter = Terminals {
            herdr: executable.clone(),
            ..Terminals::default()
        };
        let pane = json!({"backend":"herdr","session":"fixture","pane_id":"pane","terminal_id":"generation"});
        let environment = [
            (
                "HOME".into(),
                crate::test_support::path("/tmp/fixture").into(),
            ),
            ("PATH".into(), crate::test_support::path("/usr/bin").into()),
            ("BOT_TOKEN".into(), "private".into()),
            ("TMUX".into(), "private".into()),
        ]
        .into_iter()
        .collect();
        let command = native_command(
            &adapter,
            &pane,
            &json!({}),
            &AttachContext::default(),
            &environment,
        )
        .unwrap();
        assert_eq!(command.get_program(), executable.as_os_str());
        assert_eq!(
            command.get_args().collect::<Vec<_>>(),
            vec!["--session", "fixture", "terminal", "attach", "generation"]
        );
        assert!(
            !command
                .get_envs()
                .any(|(key, _)| key == "BOT_TOKEN" || key == "TMUX")
        );
        let mut stale = pane;
        stale["terminal_id"] = json!("old-generation");
        assert!(
            native_command(
                &adapter,
                &stale,
                &json!({}),
                &AttachContext::default(),
                &environment
            )
            .is_err()
        );
        fs::remove_dir_all(directory).unwrap();
    }
    #[test]
    fn herdr_create_and_organize_verify_grouping_and_sanitize_new_shell() {
        use std::{fs, os::unix::fs::PermissionsExt};
        let directory =
            std::env::temp_dir().join(format!("daedalus-workspace-actions-{}", std::process::id()));
        fs::create_dir(&directory).unwrap();
        let pane = json!({"backend":"herdr","session":"fixture","server":"fixture","pane_id":"original","terminal_id":"generation","workspace_id":"workspace","tab_id":"tab","cwd":directory});
        fs::write(
            directory.join("pane.json"),
            serde_json::to_vec(&json!({"pane":pane})).unwrap(),
        )
        .unwrap();
        let created = json!({"pane_id":"created","terminal_id":"created-generation","workspace_id":"new-workspace","tab_id":"new-tab","cwd":directory});
        fs::write(
            directory.join("created.json"),
            serde_json::to_vec(&json!({"pane":created})).unwrap(),
        )
        .unwrap();
        fs::write(
            directory.join("response.json"),
            serde_json::to_vec(&json!({"root_pane":created})).unwrap(),
        )
        .unwrap();
        let executable = directory.join("herdr");
        let script = format!(
            "#!/bin/sh\ncd '{}' || exit 1\ncase \"$3:$4\" in\npane:get) if [ \"$5\" = created ]; then cat created.json; else cat pane.json; fi ;;\nworkspace:create|tab:create) printf '%s\\n' \"$@\" > mutation; cat response.json ;;\nworkspace:rename|tab:rename|workspace:close|tab:close) printf '%s\\n' \"$@\" > mutation; printf '{{}}\\n' ;;\npane:process-info) printf '%s\\n' '{{\"process_info\":{{\"shell_pid\":17,\"foreground_processes\":[{{\"pid\":17}}]}}}}' ;;\npane:run) printf '%s' \"$6\" > command; printf '{{}}\\n' ;;\nesac\n",
            directory.display()
        );
        fs::write(&executable, script).unwrap();
        fs::set_permissions(&executable, fs::Permissions::from_mode(0o700)).unwrap();
        let adapter = Terminals {
            herdr: executable,
            ..Terminals::default()
        };
        let mut changed = pane.clone();
        changed["workspace_id"] = json!("stale-workspace");
        assert!(organize(&adapter, &changed, "close-project", "", None).is_err());
        assert!(!directory.join("mutation").exists());
        let label = "$(touch never-created) 'literal'";
        assert_eq!(
            organize(&adapter, &pane, "rename-group", label, None)
                .unwrap()
                .state,
            ReceiptState::Accepted
        );
        assert!(
            fs::read_to_string(directory.join("mutation"))
                .unwrap()
                .contains(label)
        );
        assert!(!directory.join("never-created").exists());
        let environment = [
            ("HOME".into(), directory.to_string_lossy().into_owned()),
            (
                "PATH".into(),
                crate::test_support::path("/usr/bin:/bin").into(),
            ),
            ("BOT_TOKEN".into(), "private-secret".into()),
        ]
        .into_iter()
        .collect();
        let config = ControlConfig::fresh(&directory).unwrap();
        let launch = launch_plan("shell", &directory, &config, &environment).unwrap();
        let receipt = create(&adapter, Backend::Herdr, "project", Some(&pane), &launch).unwrap();
        assert_eq!(receipt.state, ReceiptState::Submitted);
        assert_eq!(
            receipt.terminal.as_ref().unwrap()["terminal_id"],
            "created-generation"
        );
        let command = fs::read_to_string(directory.join("command")).unwrap();
        assert!(command.contains("exec '/usr/bin/env' '-i'"));
        assert!(!command.contains("BOT_TOKEN"));
        assert!(!command.contains("private-secret"));
        fs::write(
            directory.join("response.json"),
            b"{\"root_pane\":{\"pane_id\":\"unknown\"}}",
        )
        .unwrap();
        assert_eq!(
            create(&adapter, Backend::Herdr, "other", Some(&pane), &launch)
                .unwrap()
                .state,
            ReceiptState::Unconfirmed
        );
        fs::remove_dir_all(directory).unwrap();
    }
    #[test]
    fn tmux_project_creation_uses_literal_argv_clean_environment_and_exact_result() {
        use std::{fs, os::unix::fs::PermissionsExt};
        let directory =
            std::env::temp_dir().join(format!("daedalus-tmux-project-{}", std::process::id()));
        fs::create_dir(&directory).unwrap();
        fs::create_dir_all(directory.join("proc/17")).unwrap();
        fs::write(
            directory.join("proc/17/stat"),
            format!("17 (sh) {}", vec!["1"; 20].join(" ")),
        )
        .unwrap();
        fs::create_dir_all(directory.join("proc/18")).unwrap();
        fs::copy(
            directory.join("proc/17/stat"),
            directory.join("proc/18/stat"),
        )
        .unwrap();
        let row = [
            "project",
            "$1",
            "shell",
            "@1",
            "%1",
            "17",
            "sh",
            "1",
            crate::test_support::path("/tmp"),
            "18",
            "shell",
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
        .join("\t");
        fs::write(directory.join("panes"), format!("{row}\n")).unwrap();
        let executable = directory.join("tmux");
        fs::write(&executable,format!("#!/bin/sh\ncd '{}' || exit 1\nif [ \"$1\" = new-session ]; then printf '%s\\n' \"$@\" > args; /usr/bin/env > environment; printf '/tmp/fixture.sock\\t%%1\\n'; elif [ \"$3\" = list-panes ]; then cat panes; fi\n",directory.display())).unwrap();
        fs::set_permissions(&executable, fs::Permissions::from_mode(0o700)).unwrap();
        let adapter = Terminals {
            tmux: executable,
            proc_root: directory.join("proc"),
            ..Default::default()
        };
        let environment = [
            ("HOME".into(), directory.to_string_lossy().into_owned()),
            (
                "PATH".into(),
                crate::test_support::path("/usr/bin:/bin").into(),
            ),
            ("BOT_TOKEN".into(), "private-secret".into()),
        ]
        .into_iter()
        .collect();
        let mut launch = launch_plan(
            "shell",
            &directory,
            &ControlConfig::fresh(&directory).unwrap(),
            &environment,
        )
        .unwrap();
        launch.args = vec!["literal $(touch never-created) 'quoted'".into()];
        let result = create(&adapter, Backend::Tmux, "project", None, &launch).unwrap();
        assert_eq!(result.state, ReceiptState::Submitted);
        assert_eq!(result.terminal.unwrap()["pane_id"], "%1");
        let args = fs::read_to_string(directory.join("args")).unwrap();
        assert!(args.contains(crate::test_support::path("/usr/bin/env\n-i\n")));
        assert!(args.contains(&launch.args[0]));
        assert!(
            !fs::read_to_string(directory.join("environment"))
                .unwrap()
                .contains("BOT_TOKEN")
        );
        assert!(!directory.join("never-created").exists());
        fs::write(
            directory.join("panes"),
            row.replace("project", "wrong-project"),
        )
        .unwrap();
        assert_eq!(
            create(&adapter, Backend::Tmux, "project", None, &launch)
                .unwrap()
                .state,
            ReceiptState::Unconfirmed
        );
        fs::remove_dir_all(directory).unwrap();
    }
}
