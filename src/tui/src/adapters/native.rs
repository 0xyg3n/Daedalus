//! Native PTY observations and reviewed launches, shared by terminal adapters.
use crate::{
    app::native_terminal::{self, SessionInfo, StartOptions},
    core::checkpoints::LaunchPlan,
};
use serde_json::{Value, json};

pub fn pane(info: &SessionInfo) -> Value {
    json!({"backend":"native", "server":"local", "session":info.id,
        "pane_id":info.id, "terminal_id":info.generation, "pid":info.pid.to_string(),
        "shell_pid":info.pid, "foreground_pid":info.foreground_pid,
        "workspace_id":info.workspace, "workspace_label":info.workspace,
        "session_id":info.workspace, "window_id":info.id, "tab_id":info.id,
        "window":info.name, "title":info.name, "agent":info.command,
        "cwd":info.cwd, "width":info.cols, "height":info.rows,
        "active":"1", "dead":"0", "controlled":info.controlled})
}
pub fn start(launch: &LaunchPlan, workspace: &str, name: &str) -> Result<Value, String> {
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
        cols: 120,
        rows: 32,
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
