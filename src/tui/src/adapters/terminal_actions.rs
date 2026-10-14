//! Exact-target terminal operations. Receipts describe submission, not execution
//! completion; transport failure after mutation is never safe to retry blindly.
use super::terminals::{Terminals, identity};
use crate::core::{checkpoints::LaunchPlan, identity::Backend};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::Path;

type Result<T> = std::result::Result<T, String>;
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReceiptState {
    Accepted,
    Submitted,
    Unconfirmed,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Receipt {
    pub state: ReceiptState,
    pub message: String,
    pub terminal: Option<Value>,
}
impl Receipt {
    fn accepted(message: &str) -> Self {
        Self {
            state: ReceiptState::Accepted,
            message: message.into(),
            terminal: None,
        }
    }
    fn submitted() -> Self {
        Self {
            state: ReceiptState::Submitted,
            message: "Input submitted; completion is not confirmed".into(),
            terminal: None,
        }
    }
    fn uncertain(message: &str) -> Self {
        Self {
            state: ReceiptState::Unconfirmed,
            message: message.into(),
            terminal: None,
        }
    }
}
#[derive(Debug, Clone)]
pub enum Action {
    Send(String),
    Keys(Vec<String>),
    Interrupt,
    Close,
    Focus,
    Rename(String),
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Placement {
    Split,
    Window,
}
fn field<'a>(value: &'a Value, key: &str) -> Result<&'a str> {
    value
        .get(key)
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| format!("Terminal lacks required {key}"))
}
fn validate_text(text: &str) -> Result<()> {
    if text.trim().is_empty() || text.len() > 64_000 || text.contains('\0') {
        return Err("Supply nonempty text within 64000 bytes, without NUL".into());
    }
    Ok(())
}
fn validate_keys(keys: &[String]) -> Result<()> {
    if keys.is_empty()
        || keys.len() > 64
        || keys.iter().any(|k| {
            k.is_empty()
                || k.len() > 40
                || k.starts_with('-')
                || !k
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"-+_".contains(&b))
        })
    {
        return Err("Invalid key sequence".into());
    }
    Ok(())
}
fn validate_environment(environment: &std::collections::BTreeMap<String, String>) -> Result<()> {
    let home = environment
        .get("HOME")
        .ok_or("Launch requires an explicit HOME")?;
    let path = environment
        .get("PATH")
        .ok_or("Launch requires an explicit PATH")?;
    if !Path::new(home).is_absolute()
        || !Path::new(home).is_dir()
        || path.is_empty()
        || std::env::split_paths(path).any(|p| !p.is_absolute())
    {
        return Err(
            "Launch requires an existing absolute HOME and an absolute PATH search list".into(),
        );
    }
    if environment.len() > 128
        || environment.iter().any(|(key, value)| {
            key.is_empty()
                || key.len() > 128
                || !key
                    .bytes()
                    .next()
                    .is_some_and(|b| b.is_ascii_alphabetic() || b == b'_')
                || !key.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
                || value.contains('\0')
                || value.len() > 64_000
        })
    {
        return Err("Invalid launch environment".into());
    }
    Ok(())
}
impl Terminals {
    pub fn capture(&self, saved: &Value, lines: usize) -> Result<String> {
        self.capture_formatted(saved, lines, false)
    }
    /// Preserve terminal colors only for the VT preview renderer. Plain-text
    /// exports and checkpoint excerpts continue to use capture().
    pub fn preview(&self, saved: &Value, lines: usize) -> Result<String> {
        self.capture_formatted(saved, lines, true)
    }
    fn capture_formatted(&self, saved: &Value, lines: usize, ansi: bool) -> Result<String> {
        let actual = self.verify(saved)?;
        let id = identity(&actual)?;
        let lines = lines.clamp(1, 500).to_string();
        let start = format!("-{lines}");
        let (binary, mut args) = match id.backend {
            Backend::Tmux => (
                &self.tmux,
                vec![
                    "-S",
                    &id.endpoint,
                    "capture-pane",
                    "-p",
                    "-t",
                    &id.pane,
                    "-S",
                    &start,
                ],
            ),
            Backend::Herdr => (
                &self.herdr,
                vec![
                    "--session",
                    &id.endpoint,
                    "pane",
                    "read",
                    &id.pane,
                    "--source",
                    "recent-unwrapped",
                    "--lines",
                    &lines,
                ],
            ),
        };
        if ansi {
            args.push(if id.backend == Backend::Tmux {
                "-e"
            } else {
                "--ansi"
            });
        }
        let text = self.query(binary, &args)?;
        self.verify(saved)?;
        Ok(text)
    }
    pub fn action(&self, saved: &Value, action: Action) -> Result<Receipt> {
        match &action {
            Action::Send(s) | Action::Rename(s) => validate_text(s)?,
            Action::Keys(k) => validate_keys(k)?,
            _ => (),
        }
        let actual = self.verify(saved)?;
        let id = identity(&actual)?;
        if id.backend == Backend::Tmux {
            let run = |args: &[&str]| {
                let mut argv = vec!["-S", id.endpoint.as_str()];
                argv.extend_from_slice(args);
                self.query(&self.tmux, &argv)
            };
            match action {
                Action::Send(text) => {
                    if run(&["send-keys", "-t", &id.pane, "-l", "--", &text]).is_err() {
                        return Ok(Receipt::uncertain(
                            "Input transfer is unconfirmed; inspect the terminal before retrying",
                        ));
                    }
                    if self.verify(saved).is_err() {
                        return Ok(Receipt::uncertain(
                            "Terminal changed after input transfer; Enter was not sent",
                        ));
                    }
                    if run(&["send-keys", "-t", &id.pane, "Enter"]).is_err() {
                        return Ok(Receipt::uncertain(
                            "Enter submission is unconfirmed; inspect the terminal before retrying",
                        ));
                    }
                    Ok(Receipt::submitted())
                }
                Action::Keys(keys) => {
                    let mut args = vec!["send-keys", "-t", &id.pane];
                    args.extend(keys.iter().map(String::as_str));
                    ack(run(&args))
                }
                Action::Interrupt => ack(run(&["send-keys", "-t", &id.pane, "C-c"])),
                Action::Close => ack(run(&["kill-pane", "-t", &id.pane])),
                Action::Rename(text) => ack(run(&["select-pane", "-t", &id.pane, "-T", &text])),
                Action::Focus => {
                    let target = format!(
                        "{}:{}",
                        field(&actual, "session_id")?,
                        field(&actual, "window_id")?
                    );
                    if run(&["select-window", "-t", &target]).is_err()
                        || self.verify(saved).is_err()
                    {
                        return Ok(Receipt::uncertain(
                            "Window focus is unconfirmed; pane focus was not submitted",
                        ));
                    }
                    ack(run(&["select-pane", "-t", &id.pane]))
                }
            }
        } else {
            let run = |args: &[&str]| self.herdr_result(&id.endpoint, args);
            match action {
                Action::Send(text) => {
                    if actual
                        .get("agent")
                        .is_some_and(|v| v.as_str().is_some_and(|s| !s.is_empty()))
                    {
                        if run(&["agent", "prompt", &id.pane, &text]).is_err() {
                            return Ok(Receipt::uncertain(
                                "Prompt submission is unconfirmed; inspect the terminal before retrying",
                            ));
                        }
                    } else {
                        if run(&["pane", "send-text", &id.pane, &text]).is_err() {
                            return Ok(Receipt::uncertain(
                                "Input transfer is unconfirmed; inspect the terminal before retrying",
                            ));
                        }
                        if self.verify(saved).is_err() {
                            return Ok(Receipt::uncertain(
                                "Terminal changed after input transfer; Enter was not sent",
                            ));
                        }
                        if run(&["pane", "send-keys", &id.pane, "enter"]).is_err() {
                            return Ok(Receipt::uncertain(
                                "Enter submission is unconfirmed; inspect the terminal before retrying",
                            ));
                        }
                    }
                    Ok(Receipt::submitted())
                }
                Action::Keys(keys) => {
                    let mut args = vec!["pane", "send-keys", &id.pane];
                    args.extend(keys.iter().map(String::as_str));
                    ack(run(&args))
                }
                Action::Interrupt => ack(run(&["pane", "send-keys", &id.pane, "ctrl+c"])),
                Action::Close => ack(run(&["pane", "close", &id.pane])),
                Action::Rename(text) => ack(run(&["pane", "rename", &id.pane, &text])),
                Action::Focus => {
                    let workspace = field(&actual, "workspace_id")?;
                    let tab = field(&actual, "tab_id")?;
                    if run(&["workspace", "focus", workspace]).is_err()
                        || self.verify(saved).is_err()
                    {
                        return Ok(Receipt::uncertain(
                            "Workspace focus is unconfirmed; tab focus was not submitted",
                        ));
                    }
                    if run(&["tab", "focus", tab]).is_err() {
                        return Ok(Receipt::uncertain("Tab focus is unconfirmed"));
                    }
                    Ok(Receipt::accepted(
                        "Workspace/tab focused. Select the inner pane in the Herdr client.",
                    ))
                }
            }
        }
    }
    pub fn spawn(
        &self,
        parent: &Value,
        launch: &LaunchPlan,
        placement: Placement,
    ) -> Result<Receipt> {
        validate_launch(launch)?;
        let actual = self.verify(parent)?;
        let id = identity(&actual)?;
        let binary = launch
            .binary
            .to_str()
            .ok_or("Executable path is not UTF-8")?;
        let cwd = launch
            .cwd
            .to_str()
            .ok_or("Working directory is not UTF-8")?;
        if id.backend == Backend::Tmux {
            let target = if placement == Placement::Window {
                format!("{}:", field(&actual, "session_id")?)
            } else {
                id.pane.clone()
            };
            let mut args = vec![
                "-S".into(),
                id.endpoint.clone(),
                if placement == Placement::Window {
                    "new-window"
                } else {
                    "split-window"
                }
                .into(),
                "-d".into(),
                "-t".into(),
                target,
                "-c".into(),
                cwd.into(),
                "-P".into(),
                "-F".into(),
                "#{pane_id}".into(),
            ];
            if placement == Placement::Split {
                args.push("-v".into());
            }
            // Multiple command arguments select tmux's direct executable mode.
            // Clear the server environment before installing the complete,
            // explicitly sanitized launch environment. Never inherit bot tokens.
            args.extend(["--".into(), "/usr/bin/env".into(), "-i".into()]);
            for (key, value) in &launch.environment {
                args.push(format!("{key}={value}"));
            }
            args.push(binary.into());
            args.extend(launch.args.clone());
            let pane = match self.query(
                &self.tmux,
                &args.iter().map(String::as_str).collect::<Vec<_>>(),
            ) {
                Ok(p) => p.trim().to_owned(),
                Err(_) => {
                    return Ok(Receipt::uncertain(
                        "Pane creation is unconfirmed; inspect the workspace before retrying",
                    ));
                }
            };
            let inventory = match self.tmux_inventory(&id.endpoint) {
                Ok(i) => i,
                Err(_) => {
                    return Ok(Receipt::uncertain(
                        "Pane was requested but its identity could not be confirmed",
                    ));
                }
            };
            let created = inventory["panes"]
                .as_array()
                .into_iter()
                .flatten()
                .find(|p| p["pane_id"] == pane && p["terminal_id"] != actual["terminal_id"])
                .cloned();
            match created {Some(terminal)=>Ok(Receipt {state:ReceiptState::Submitted,message:"Launch submitted in a new pane; login and session availability are not confirmed".into(),terminal:Some(terminal)}),None=>Ok(Receipt::uncertain("New pane identity is unconfirmed; inspect the workspace before retrying"))}
        } else {
            if placement != Placement::Split {
                return Err("Herdr session launches require a split destination".into());
            }
            let split = match self.herdr_result(
                &id.endpoint,
                &[
                    "pane",
                    "split",
                    "--pane",
                    &id.pane,
                    "--direction",
                    "down",
                    "--cwd",
                    cwd,
                    "--no-focus",
                ],
            ) {
                Ok(v) => v,
                Err(_) => {
                    return Ok(Receipt::uncertain(
                        "Pane creation is unconfirmed; inspect the workspace before retrying",
                    ));
                }
            };
            let mut pane = match split.get("pane").and_then(Value::as_object).cloned() {
                Some(p) => p,
                None => return Ok(Receipt::uncertain("New pane identity is missing")),
            };
            pane.insert("backend".into(), "herdr".into());
            pane.insert("session".into(), id.endpoint.clone().into());
            let pane = Value::Object(pane);
            let created = identity(&pane)?;
            if created.generation == id.generation {
                return Ok(Receipt::uncertain("Server did not create a new terminal"));
            }
            self.launch_created_herdr(&pane, launch)
        }
    }
    /// Starts an explicitly returned new Herdr terminal only while its foreground
    /// process is still its own shell. Never use this to inject into an old agent.
    pub fn launch_created_herdr(&self, pane: &Value, launch: &LaunchPlan) -> Result<Receipt> {
        validate_launch(launch)?;
        let created = identity(pane)?;
        if created.backend != Backend::Herdr {
            return Err("Expected a new Herdr shell".into());
        }
        let run = (|| -> Result<()> {
            self.verify(pane)?;
            let info = self.herdr_result(
                &created.endpoint,
                &["pane", "process-info", "--pane", &created.pane],
            )?;
            let info = info
                .get("process_info")
                .ok_or("Missing process information")?;
            let processes = info
                .get("foreground_processes")
                .and_then(Value::as_array)
                .ok_or("Missing foreground process information")?;
            if processes.len() != 1
                || processes[0].get("pid").is_none()
                || processes[0].get("pid") != info.get("shell_pid")
            {
                return Err("New terminal is no longer an available shell".into());
            }
            self.verify(pane)?;
            let mut words = vec!["/usr/bin/env".to_owned(), "-i".to_owned()];
            for (key, value) in &launch.environment {
                words.push(format!("{key}={value}"));
            }
            words.push(
                launch
                    .binary
                    .to_str()
                    .ok_or("Executable path is not UTF-8")?
                    .into(),
            );
            words.extend(launch.args.clone());
            let command = words
                .iter()
                .map(|w| shell_quote(w))
                .collect::<Vec<_>>()
                .join(" ");
            let command = format!(
                "cd {} && exec {}",
                shell_quote(launch.cwd.to_str().ok_or("Project path is not UTF-8")?),
                command
            );
            self.herdr_result(&created.endpoint, &["pane", "run", &created.pane, &command])?;
            Ok(())
        })();
        Ok(Receipt {
            state: if run.is_ok() {
                ReceiptState::Submitted
            } else {
                ReceiptState::Unconfirmed
            },
            message: if run.is_ok() {
                "Launch submitted; the harness assigns its child session ID"
            } else {
                "New pane exists but launch is unconfirmed; inspect it before retrying"
            }
            .into(),
            terminal: Some(pane.clone()),
        })
    }
}
pub(crate) fn validate_launch(launch: &LaunchPlan) -> Result<()> {
    validate_environment(&launch.environment)?;
    if !launch.binary.is_absolute()
        || !launch.cwd.is_dir()
        || launch.args.len() > 128
        || launch
            .args
            .iter()
            .any(|a| a.len() > 64000 || a.contains('\0'))
    {
        return Err("Invalid native launch plan".into());
    }
    Ok(())
}

fn ack<T>(result: Result<T>) -> Result<Receipt> {
    Ok(match result {
        Ok(_) => Receipt::accepted("Action accepted"),
        Err(_) => Receipt::uncertain(
            "Action outcome is unconfirmed; inspect the terminal before retrying",
        ),
    })
}
fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\"'\"'"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn previews_preserve_rgb_but_exports_stay_plain() {
        use std::{fs, os::unix::fs::PermissionsExt};
        let dir =
            std::env::temp_dir().join(format!("daedalus-color-preview-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let binary = dir.join("herdr");
        fs::write(
            &binary,
            r#"#!/bin/sh
case "$4" in
get) printf '%s\n' '{"pane":{"pane_id":"pane","terminal_id":"generation"}}' ;;
read)
  for arg in "$@"; do
    if [ "$arg" = '--ansi' ]; then printf '\033[38;2;12;34;56mcolored\033[0m\n'; exit; fi
  done
  printf 'colored\n' ;;
*) exit 1 ;;
esac
"#,
        )
        .unwrap();
        fs::set_permissions(&binary, fs::Permissions::from_mode(0o700)).unwrap();
        let adapter = Terminals {
            herdr: binary,
            ..Terminals::default()
        };
        let pane = json!({"backend":"herdr","session":"fixture","pane_id":"pane","terminal_id":"generation"});
        assert_eq!(adapter.capture(&pane, 20).unwrap(), "colored\n");
        assert_eq!(
            adapter.preview(&pane, 20).unwrap(),
            "\x1b[38;2;12;34;56mcolored\x1b[0m\n"
        );
        fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn herdr_spawn_clears_server_environment_and_preserves_literal_values() {
        use std::{collections::BTreeMap, fs, os::unix::fs::PermissionsExt};
        let dir =
            std::env::temp_dir().join(format!("daedalus-herdr-spawn-mock-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let output = dir.join("child-output");
        let command_log = dir.join("command");
        let child = dir.join("fake-agent");
        fs::write(&child, format!("#!/bin/sh\nprintf '%s\\n' \"${{BOT_SECRET-unset}}\" \"$HOME\" \"$TERM\" \"$LANG\" \"$1\" > '{}'\n",output.display())).unwrap();
        fs::set_permissions(&child, fs::Permissions::from_mode(0o700)).unwrap();
        let executable = dir.join("herdr");
        let script = format!(
            "#!/bin/sh\nexport BOT_SECRET=server-only\ncase \"$4\" in\nget) if [ \"$5\" = parent ]; then printf '%s\\n' '{{\"pane\":{{\"pane_id\":\"parent\",\"terminal_id\":\"old\"}}}}'; else printf '%s\\n' '{{\"pane\":{{\"pane_id\":\"new\",\"terminal_id\":\"new-uuid\"}}}}'; fi ;;\nsplit) printf '%s\\n' '{{\"pane\":{{\"pane_id\":\"new\",\"terminal_id\":\"new-uuid\"}}}}' ;;\nprocess-info) printf '%s\\n' '{{\"process_info\":{{\"shell_pid\":77,\"foreground_processes\":[{{\"pid\":77}}]}}}}' ;;\nrun) printf '%s' \"$6\" > '{}'; /bin/sh -c \"$6\" || exit 1; printf '{{}}\\n' ;;\nesac\n",
            command_log.display()
        );
        fs::write(&executable, script).unwrap();
        fs::set_permissions(&executable, fs::Permissions::from_mode(0o700)).unwrap();
        let adapter = Terminals {
            herdr: executable,
            ..Terminals::default()
        };
        let literal = "$(touch never); 'literal value'";
        let launch = LaunchPlan {
            binary: child,
            args: vec![literal.into()],
            environment: BTreeMap::from([
                ("HOME".into(), dir.to_string_lossy().into_owned()),
                ("PATH".into(), "/usr/bin:/bin".into()),
                ("TERM".into(), "xterm-256color".into()),
                ("LANG".into(), literal.into()),
            ]),
            cwd: dir.clone(),
            operation: crate::core::checkpoints::Operation::Resume,
        };
        let parent =
            json!({"backend":"herdr","session":"work","pane_id":"parent","terminal_id":"old"});
        let receipt = adapter.spawn(&parent, &launch, Placement::Split).unwrap();
        assert_eq!(receipt.state, ReceiptState::Submitted);
        let captured = fs::read_to_string(output).unwrap();
        let rows: Vec<_> = captured.lines().collect();
        assert_eq!(
            rows,
            vec![
                "unset",
                dir.to_str().unwrap(),
                "xterm-256color",
                literal,
                literal
            ]
        );
        assert!(
            fs::read_to_string(command_log)
                .unwrap()
                .contains(" && exec '/usr/bin/env' '-i' ")
        );
        fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn spawn_environment_must_be_explicit_and_well_formed() {
        use std::collections::BTreeMap;
        assert!(validate_environment(&BTreeMap::new()).is_err());
        let mut env = BTreeMap::from([
            (
                "HOME".into(),
                std::env::temp_dir().to_string_lossy().into_owned(),
            ),
            ("PATH".into(), "/usr/bin".into()),
        ]);
        assert!(validate_environment(&env).is_ok());
        env.insert("BAD=KEY".into(), "value".into());
        assert!(validate_environment(&env).is_err());
        env.remove("BAD=KEY");
        env.insert("PATH".into(), ".:/usr/bin".into());
        assert!(validate_environment(&env).is_err());
    }
    #[test]
    fn tmux_spawn_preserves_argument_boundaries() {
        use std::{collections::BTreeMap, fs, os::unix::fs::PermissionsExt};
        let dir =
            std::env::temp_dir().join(format!("daedalus-tmux-spawn-mock-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let proc_root = dir.join("proc");
        for pid in ["11", "22", "33"] {
            fs::create_dir_all(proc_root.join(pid)).unwrap();
            let mut fields = vec!["0"; 20];
            fields[19] = "100";
            fs::write(
                proc_root.join(pid).join("stat"),
                format!("{pid} (test) {}", fields.join(" ")),
            )
            .unwrap();
        }
        let fixture = |pane, pid| {
            [
                "work",
                "$1",
                "shell",
                "@1",
                pane,
                pid,
                "sh",
                "1",
                "/tmp",
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
        };
        let executable = dir.join("tmux");
        let marker = dir.join("created");
        let log = dir.join("args");
        let script = format!(
            "#!/bin/sh\ncase \"$3\" in\nlist-panes) printf '%s\\n' '{}'; if [ -f '{}' ]; then printf '%s\\n' '{}'; fi ;;\nsplit-window) printf '%s\\0' \"$@\" > '{}'; : > '{}'; printf '%%2\\n' ;;\nesac\n",
            fixture("%1", "22"),
            marker.display(),
            fixture("%2", "33"),
            log.display(),
            marker.display()
        );
        fs::write(&executable, script).unwrap();
        fs::set_permissions(&executable, fs::Permissions::from_mode(0o700)).unwrap();
        let adapter = Terminals {
            tmux: executable,
            proc_root,
            ..Terminals::default()
        };
        let pane = json!({"backend":"tmux","socket":"/tmp/mock.sock","pane_id":"%1","terminal_id":"v2:11:100:22:100","session_id":"$1"});
        let literal = "$(touch never); a b";
        let launch = LaunchPlan {
            binary: "/opt/example-agent".into(),
            args: vec![literal.into()],
            environment: BTreeMap::from([
                ("HOME".into(), dir.to_string_lossy().into_owned()),
                ("PATH".into(), "/usr/bin:/bin".into()),
                ("TERM".into(), "xterm-256color".into()),
                ("LANG".into(), "$(touch never) 'literal'".into()),
            ]),
            cwd: dir.clone(),
            operation: crate::core::checkpoints::Operation::Resume,
        };
        let receipt = adapter.spawn(&pane, &launch, Placement::Split).unwrap();
        assert_eq!(receipt.state, ReceiptState::Submitted);
        assert_eq!(receipt.terminal.unwrap()["pane_id"], "%2");
        let bytes = fs::read(log).unwrap();
        let args: Vec<_> = bytes.split(|b| *b == 0).collect();
        assert!(args.contains(&literal.as_bytes()));
        assert!(args.contains(&b"/usr/bin/env".as_slice()));
        assert!(args.contains(&b"-i".as_slice()));
        assert!(args.contains(&b"LANG=$(touch never) 'literal'".as_slice()));
        assert!(!args.contains(&b"-e".as_slice()));
        fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn tmux_revalidates_before_enter_and_sends_literal_text() {
        use std::{fs, os::unix::fs::PermissionsExt};
        let dir =
            std::env::temp_dir().join(format!("daedalus-tmux-send-mock-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let proc_root = dir.join("proc");
        for pid in ["11", "22", "23"] {
            fs::create_dir_all(proc_root.join(pid)).unwrap();
            let mut fields = vec!["0"; 20];
            fields[19] = "100";
            fs::write(
                proc_root.join(pid).join("stat"),
                format!("{pid} (test) {}", fields.join(" ")),
            )
            .unwrap();
        }
        let marker = dir.join("changed");
        let log = dir.join("log");
        let executable = dir.join("tmux");
        let fixture = |pid| {
            [
                "work",
                "$1",
                "shell",
                "@1",
                "%1",
                pid,
                "sh",
                "1",
                "/tmp",
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
        };
        let script = format!(
            "#!/bin/sh\ncase \"$3\" in\nlist-panes) if [ -f '{}' ]; then printf '%s\\n' '{}'; else printf '%s\\n' '{}'; fi ;;\nsend-keys) if [ \"$6\" = -l ]; then printf '%s' \"$8\" > '{}'; : > '{}'; else printf ENTER >> '{}'; fi ;;\nesac\n",
            marker.display(),
            fixture("23"),
            fixture("22"),
            log.display(),
            marker.display(),
            log.display()
        );
        fs::write(&executable, script).unwrap();
        fs::set_permissions(&executable, fs::Permissions::from_mode(0o700)).unwrap();
        let adapter = Terminals {
            tmux: executable,
            proc_root,
            ..Terminals::default()
        };
        let pane = json!({"backend":"tmux","socket":"/tmp/mock.sock","pane_id":"%1","terminal_id":"v2:11:100:22:100","session_id":"$1"});
        let text = "$(touch should-not-exist); 'quoted'";
        let receipt = adapter.action(&pane, Action::Send(text.into())).unwrap();
        assert_eq!(receipt.state, ReceiptState::Unconfirmed);
        assert_eq!(fs::read_to_string(log).unwrap(), text);
        fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn invalid_keys_cannot_inject_options() {
        assert!(validate_keys(&["-a".into()]).is_err());
        assert!(validate_keys(&["Enter".into(), "C-c".into()]).is_ok());
    }
    #[test]
    fn shell_quote_keeps_commands_literal() {
        assert_eq!(shell_quote("a'$(touch x)"), "'a'\"'\"'$(touch x)'");
    }
    #[test]
    fn identity_change_after_text_prevents_enter() {
        use std::{fs, os::unix::fs::PermissionsExt};
        let dir = std::env::temp_dir().join(format!("daedalus-send-mock-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let script = dir.join("herdr");
        let marker = dir.join("changed");
        let log = dir.join("log");
        let content = format!(
            "#!/bin/sh\nif [ \"$4\" = get ]; then\n if [ -f '{}' ]; then id=new; else id=old; fi\n printf '{{\"pane\":{{\"pane_id\":\"pane\",\"terminal_id\":\"%s\"}}}}\\n' \"$id\"\nelif [ \"$4\" = send-text ]; then\n : > '{}'\n printf 'text\\n' >> '{}'\nelif [ \"$4\" = send-keys ]; then\n printf 'ENTER\\n' >> '{}'\nfi\n",
            marker.display(),
            marker.display(),
            log.display(),
            log.display()
        );
        fs::write(&script, content).unwrap();
        fs::set_permissions(&script, fs::Permissions::from_mode(0o700)).unwrap();
        let adapter = Terminals {
            herdr: script,
            ..Terminals::default()
        };
        let pane = json!({"backend":"herdr","session":"work","pane_id":"pane","terminal_id":"old"});
        let receipt = adapter.action(&pane, Action::Send("hello".into())).unwrap();
        assert_eq!(receipt.state, ReceiptState::Unconfirmed);
        assert_eq!(fs::read_to_string(log).unwrap(), "text\n");
        fs::remove_dir_all(dir).unwrap();
    }
}
