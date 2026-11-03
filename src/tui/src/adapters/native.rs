//! Native PTY observations and reviewed launches, shared by terminal adapters.
use crate::{
    app::native_terminal::{self, SessionInfo, StartOptions},
    core::checkpoints::LaunchPlan,
};
use serde_json::{Value, json};

pub fn pane(info: &SessionInfo) -> Value {
    let launch = info.launch_session.as_ref().filter(|session| {
        info.exited.is_none()
            && info.foreground_pid.is_none_or(|pid| pid == info.pid)
            && super::harnesses::canonical_id(&info.command) == Some(session.harness.as_str())
    });
    json!({"backend":"native", "server":"local", "session":info.id,
        "pane_id":info.id, "terminal_id":info.generation, "pid":info.pid.to_string(),
        "shell_pid":info.pid, "foreground_pid":info.foreground_pid,
        "workspace_id":info.workspace, "workspace_label":info.workspace,
        "session_id":info.workspace, "window_id":info.id, "tab_id":info.id,
        "window":info.name, "title":info.name,
        "agent":super::harnesses::canonical_id(&info.command).unwrap_or(&info.command),
        "agent_session": launch.map(|session| json!({"kind":"id","agent":session.harness,"value":session.id,"source":"launch"})),
        "cwd":info.cwd, "width":info.cols, "height":info.rows,
        "active":"1", "dead":"0", "controlled":info.controlled})
}

pub fn start(
    launch: &LaunchPlan,
    workspace: &str,
    name: &str,
    cols: u16,
    rows: u16,
    scrollback_rows: u64,
) -> Result<Value, String> {
    super::terminal_actions::validate_launch(launch)?;
    let mut argv = vec![
        launch
            .binary
            .to_str()
            .ok_or("Executable path is not UTF-8")?
            .to_owned(),
    ];
    argv.extend(launch.args.clone());
    let session = native_terminal::start(StartOptions {
        name: name.into(),
        workspace: workspace.into(),
        cwd: launch.cwd.clone(),
        argv,
        cols,
        rows,
        scrollback_rows,
        environment: Some(launch.environment.clone()),
    })?;
    Ok(pane(&session))
}
pub fn keys(keys: &[String]) -> Result<Vec<u8>, String> {
    let mut bytes = Vec::new();
    for key in keys {
        let key = key.to_ascii_lowercase();
        let sequence: &[u8] = match key.as_str() {
            "enter" | "return" => b"\r",
            "space" => b" ",
            "tab" => b"\t",
            "escape" | "esc" => b"\x1b",
            "backspace" | "bspace" => b"\x7f",
            "up" => b"\x1b[A",
            "down" => b"\x1b[B",
            "right" => b"\x1b[C",
            "left" => b"\x1b[D",
            "home" => b"\x1b[H",
            "end" => b"\x1b[F",
            "delete" => b"\x1b[3~",
            "pageup" | "ppage" => b"\x1b[5~",
            "pagedown" | "npage" => b"\x1b[6~",
            _ => {
                if let Some(control) = key.strip_prefix("ctrl+").or_else(|| key.strip_prefix("c-"))
                    && control.len() == 1
                    && control.as_bytes()[0].is_ascii_lowercase()
                {
                    bytes.push(control.as_bytes()[0] - b'a' + 1);
                    continue;
                }
                return Err(format!("Unsupported native terminal key: {key}"));
            }
        };
        bytes.extend_from_slice(sequence);
    }
    Ok(bytes)
}

#[cfg(test)]
mod identity_tests {
    use super::*;
    #[test]
    fn native_launch_metadata_is_private_backward_compatible_and_not_a_fork_parent() {
        let mut info: SessionInfo = serde_json::from_value(json!({"id":"pty","generation":"generation","name":"fixture","workspace":"project","cwd":".","command":"CODEX.EXE","pid":42,"foreground_pid":null,"cols":80,"rows":24,"controlled":false})).unwrap();
        assert!(super::super::terminals::reported_session(&pane(&info)).is_none());
        info.launch_session = Some(native_terminal::LaunchSession {
            harness: "codex".into(),
            id: "019a043e-1234-7000-8000-000000000001".into(),
        });
        assert_eq!(
            super::super::terminals::reported_session(&pane(&info)),
            Some("019a043e-1234-7000-8000-000000000001")
        );
        let wire = serde_json::to_value(&info).unwrap();
        assert!(wire.get("argv").is_none() && wire.get("environment").is_none());
        info.foreground_pid = Some(43);
        assert!(super::super::terminals::reported_session(&pane(&info)).is_none());
        info.foreground_pid = None;
        info.exited = Some(0);
        assert!(super::super::terminals::reported_session(&pane(&info)).is_none());
    }
}
