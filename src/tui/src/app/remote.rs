//! SSH owns authentication, transport encryption and the remote filesystem.
//! No provider credentials or local configuration are copied to the host.
use serde::{Deserialize, Serialize};
use std::{path::PathBuf, process::Command};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Target {
    pub host: String,
    pub executable: String,
    pub directory: PathBuf,
}
impl Target {
    pub fn validate(&self) -> Result<(), String> {
        if self.host.is_empty()
            || self.host.len() > 253
            || self.host.starts_with('-')
            || !self
                .host
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"._-@".contains(&b))
            || self.host.matches('@').count() > 1
        {
            return Err("Remote host must be an SSH config alias or user@host".into());
        }
        if self.executable.is_empty()
            || self.executable.starts_with('-')
            || self.executable.len() > 4096
            || self.executable.chars().any(char::is_control)
            || !self.directory.to_string_lossy().starts_with('/')
            || self
                .directory
                .to_string_lossy()
                .chars()
                .any(char::is_control)
        {
            return Err(
                "Remote executable and absolute POSIX configuration directory are required".into(),
            );
        }
        Ok(())
    }
    pub fn command(&self, argv: &[String]) -> Result<Command, String> {
        self.validate()?;
        if argv.is_empty() || argv.iter().any(|s| s.contains('\0')) {
            return Err("Invalid remote command".into());
        }
        let quote = |s: &str| format!("'{}'", s.replace('\'', "'\\''"));
        let remote = argv.iter().map(|s| quote(s)).collect::<Vec<_>>().join(" ");
        let mut command = Command::new("ssh");
        command.args([
            "-T",
            "-o",
            "BatchMode=yes",
            "-o",
            "StrictHostKeyChecking=yes",
            "-o",
            "ConnectTimeout=8",
            "-o",
            "ServerAliveInterval=15",
            "-o",
            "ServerAliveCountMax=2",
            "--",
            &self.host,
            &remote,
        ]);
        // Inherited local pane identities cannot become remote caller identities.
        for key in [
            "HERDR_ENV",
            "HERDR_PANE_ID",
            "HERDR_TAB_ID",
            "HERDR_WORKSPACE_ID",
            "TMUX",
            "TMUX_PANE",
        ] {
            command.env_remove(key);
        }
        Ok(command)
    }
    pub fn backend(&self) -> Result<Command, String> {
        self.command(&[
            self.executable.clone(),
            "backend".into(),
            "--stdio".into(),
            "--config-dir".into(),
            self.directory.to_string_lossy().into_owned(),
        ])
    }
}

/// Remove only client-owned options; the remaining options configure the local
/// interface. A remote target never guesses a local configuration on failure.
pub fn take_options(args: &mut Vec<String>) -> Result<Option<Target>, String> {
    let mut host = None;
    let mut executable = None;
    let mut directory = None;
    let mut rest = Vec::new();
    let mut i = 0;
    while i < args.len() {
        let slot = match args[i].as_str() {
            "--remote" => Some(&mut host),
            "--remote-bin" => Some(&mut executable),
            "--remote-config-dir" => Some(&mut directory),
            _ => None,
        };
        if let Some(slot) = slot {
            if slot.is_some() {
                return Err("Duplicate remote option".into());
            }
            i += 1;
            *slot = Some(args.get(i).ok_or("Missing remote option value")?.clone());
        } else {
            rest.push(args[i].clone());
        }
        i += 1;
    }
    if host.is_none() && (executable.is_some() || directory.is_some()) {
        return Err("Remote options require --remote HOST".into());
    }
    let target = host.map(|host| Target {
        host,
        executable: executable.unwrap_or_else(|| "daedalus".into()),
        directory: PathBuf::from(directory.unwrap_or_default()),
    });
    if let Some(target) = &target {
        target.validate()?;
        if rest
            .iter()
            .any(|a| matches!(a.as_str(), "--demo" | "setup" | "popup"))
        {
            return Err("Remote TUI requires an already configured remote instance; demo, setup and popup are local".into());
        }
    }
    *args = rest;
    Ok(target)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn remote_options_never_use_a_local_path_or_fall_back_to_local_execution() {
        let mut args = vec![
            "tui".into(),
            "--remote".into(),
            "dev@box".into(),
            "--remote-config-dir".into(),
            "/tmp/remote state".into(),
        ];
        let target = take_options(&mut args).unwrap().unwrap();
        assert_eq!(args, ["tui"]);
        let command = target.backend().unwrap();
        let argv = command
            .get_args()
            .map(|s| s.to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        assert!(argv.contains(&"StrictHostKeyChecking=yes".into()));
        assert!(argv.last().unwrap().contains("'--stdio'"));
        for host in ["-oProxyCommand=bad", "host;touch x", "$(id)", "host\nnext"] {
            let mut bad = target.clone();
            bad.host = host.into();
            assert!(bad.backend().is_err());
        }
        let mut missing = vec!["--remote".into(), "box".into()];
        assert!(take_options(&mut missing).is_err());
    }
    #[test]
    fn remote_stream_arguments_are_quoted_as_literals() {
        let target = Target {
            host: "box".into(),
            executable: "daedalus".into(),
            directory: "/tmp/state".into(),
        };
        let command = target
            .command(&[
                "/path/with space/client".into(),
                "a';echo BAD".into(),
                "--cols".into(),
                "120".into(),
            ])
            .unwrap();
        let line = command.get_args().last().unwrap().to_string_lossy();
        assert!(line.starts_with("'/path/with space/client' 'a'\\'';echo BAD'"));
    }
}
