//! Session references and launch plans. No process execution or credential copying.
use super::layouts::{Result, TerminalRef, check_path, read_private, write_private};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Checkpoint {
    pub name: String,
    pub harness: String,
    #[serde(default, alias = "session_id")]
    pub sid: String,
    pub cwd: String,
    #[serde(default)]
    pub profile: String,
    #[serde(default)]
    pub origin: String,
    #[serde(default, alias = "iso")]
    pub created_at: String,
    #[serde(default)]
    pub context_file: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<TerminalRef>,
}
fn valid_sid(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 200
        && s.as_bytes()[0].is_ascii_alphanumeric()
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_.:-".contains(&b))
}
impl Checkpoint {
    pub fn validate(&self) -> Result<()> {
        if self.name.trim().is_empty()
            || self.name.chars().count() > 100
            || self.name.chars().any(char::is_control)
        {
            return Err("Use a checkpoint name of 1 to 100 printable characters".into());
        }
        if self.harness.is_empty()
            || !self
                .harness
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-')
        {
            return Err("Invalid harness identity".into());
        }
        if !self.sid.is_empty() && !valid_sid(&self.sid) {
            return Err("Invalid session ID".into());
        }
        if self.sid.is_empty() && self.context_file.is_empty() {
            return Err("Checkpoint needs a session ID or captured context".into());
        }
        if !self.context_file.is_empty() {
            check_path(Path::new(&self.context_file))?;
        }
        if !Path::new(&self.cwd).is_absolute() {
            return Err("Checkpoint project directory must be absolute".into());
        }
        if !self.profile.is_empty() && !Path::new(&self.profile).is_absolute() {
            return Err("Profile path must be absolute".into());
        }
        if let Some(source) = &self.source {
            source.validate()?;
        }
        Ok(())
    }
}
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Registry {
    pub checkpoints: Vec<Checkpoint>,
}
impl Registry {
    pub fn load(directory: &Path) -> Result<Self> {
        let mut checkpoints = Vec::new();
        for name in [
            "dashboard-checkpoints.json",
            "session-checkpoints.json",
            "session_checkpoints.json",
        ] {
            let Some(bytes) = read_private(&directory.join(name))? else {
                continue;
            };
            let value: serde_json::Value =
                serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
            if name == "session_checkpoints.json" {
                let seats = value
                    .get("seats")
                    .and_then(|v| v.as_object())
                    .ok_or("Legacy checkpoint seats must be an object")?;
                for (seat, entries) in seats {
                    for entry in entries
                        .as_array()
                        .ok_or("Legacy checkpoint seat must be an array")?
                    {
                        let mut entry = entry.as_object().ok_or("Invalid checkpoint")?.clone();
                        entry.insert("name".into(), seat.clone().into());
                        entry.insert("harness".into(), "claude".into());
                        checkpoints.push(decode_legacy(serde_json::Value::Object(entry))?);
                    }
                }
            } else {
                for entry in value
                    .get("checkpoints")
                    .and_then(|v| v.as_array())
                    .ok_or("Checkpoint registry must contain an array")?
                {
                    checkpoints.push(decode_legacy(entry.clone())?);
                }
            }
        }
        // Reading old registries remains safe after the merged canonical file
        // has been saved: only byte-equivalent normalized records collapse.
        let mut seen = std::collections::BTreeSet::new();
        checkpoints.retain(|row| {
            seen.insert(serde_json::to_string(row).expect("Checkpoint serialization is infallible"))
        });
        Ok(Self { checkpoints })
    }
    /// Saves the canonical registry. Legacy records are deduplicated on reload;
    /// importers can archive the originals after their backup is verified.
    pub fn save(&self, directory: &Path) -> Result<()> {
        for checkpoint in &self.checkpoints {
            checkpoint.validate()?;
        }
        write_private(
            &directory.join("dashboard-checkpoints.json"),
            &serde_json::to_vec_pretty(self).map_err(|e| e.to_string())?,
        )
    }
    pub fn add_verified(
        &mut self,
        checkpoint: Checkpoint,
        before: &TerminalRef,
        after: &TerminalRef,
    ) -> Result<()> {
        checkpoint.validate()?;
        before.validate()?;
        after.validate()?;
        if !before.same_identity(after)
            || checkpoint
                .source
                .as_ref()
                .is_some_and(|s| !s.same_identity(before))
        {
            return Err("Terminal changed during checkpoint capture".into());
        }
        self.checkpoints.push(checkpoint);
        Ok(())
    }
}
fn decode_legacy(mut value: serde_json::Value) -> Result<Checkpoint> {
    let row = value.as_object_mut().ok_or("Invalid checkpoint object")?;
    if !row.contains_key("profile") {
        let profile = row
            .get("claude_config_dir")
            .or_else(|| row.get("grok_home"))
            .cloned()
            .unwrap_or("".into());
        row.insert("profile".into(), profile);
    }
    let checkpoint: Checkpoint = serde_json::from_value(value).map_err(|e| e.to_string())?;
    checkpoint.validate()?;
    Ok(checkpoint)
}

// Resume options may precede the positional session ID. Do not mistake a
// model/profile value or an initial prompt selected with --last for that ID.
fn codex_launch_sid(argv: &[String]) -> Option<&str> {
    if !argv
        .get(1)
        .is_some_and(|v| matches!(v.as_str(), "resume" | "fork"))
    {
        return None;
    }
    let mut args = argv[2..].iter();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--last" => return None,
            "-c"
            | "--config"
            | "-m"
            | "--model"
            | "-p"
            | "--profile"
            | "-s"
            | "--sandbox"
            | "-a"
            | "--ask-for-approval"
            | "-C"
            | "--cd"
            | "--enable"
            | "--disable"
            | "--remote"
            | "--remote-auth-token-env"
            | "--local-provider"
            | "--add-dir" => {
                args.next()?;
            }
            "--no-daemon"
            | "--all"
            | "--include-non-interactive"
            | "--strict-config"
            | "--oss"
            | "--approve-for-me"
            | "--dangerously-bypass-approvals-and-sandbox"
            | "--full-auto"
            | "--search"
            | "--no-alt-screen" => (),
            "--" => return args.next().map(String::as_str),
            value if value.starts_with('-') => return None,
            value => return Some(value),
        }
    }
    None
}

/// A fork parent's launch ID is never reported as the new child's ID.
pub fn resolve_sid(argv: &[String], harness: &str, reported: &str) -> Result<Option<String>> {
    let forking = argv.iter().any(|a| a == "--fork-session")
        || (harness == "codex" && argv.get(1).is_some_and(|a| a == "fork"));
    let mut ids = Vec::new();
    for pair in argv.windows(2) {
        if matches!(pair[0].as_str(), "--resume" | "-r" | "--session-id") {
            ids.push(pair[1].as_str());
        }
    }
    if harness == "codex" {
        if let Some(id) = codex_launch_sid(argv) {
            ids.push(id);
        } else if forking {
            return Ok(None);
        }
    }
    if forking {
        if reported.is_empty() || ids.contains(&reported) {
            return Ok(None);
        }
        if !valid_sid(reported) {
            return Err("Invalid reported session ID".into());
        }
        return Ok(Some(reported.into()));
    }
    if !reported.is_empty() {
        ids.push(reported);
    }
    ids.retain(|id| valid_sid(id));
    ids.sort_unstable();
    ids.dedup();
    match ids.len() {
        0 => Ok(None),
        1 => Ok(Some(ids[0].into())),
        _ => Err("Native metadata and launch arguments disagree on the session ID".into()),
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Operation {
    Resume,
    NativeFork,
    Handoff,
}
/// Supplied by the adapter after probing the installed version. No inferred support.
#[derive(Debug, Clone)]
pub struct Capabilities {
    pub harness: String,
    pub binary: PathBuf,
    pub resume: bool,
    pub native_fork: bool,
    pub handoff: bool,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LaunchPlan {
    pub binary: PathBuf,
    pub args: Vec<String>,
    pub environment: BTreeMap<String, String>,
    pub cwd: PathBuf,
    pub operation: Operation,
}
pub fn plan(
    checkpoint: &Checkpoint,
    operation: Operation,
    target: &Capabilities,
    context_root: &Path,
) -> Result<LaunchPlan> {
    checkpoint.validate()?;
    if !target.binary.is_absolute() {
        return Err("Harness adapter must provide an absolute executable path".into());
    }
    if !Path::new(&checkpoint.cwd).is_dir() {
        return Err("Saved project directory no longer exists".into());
    }
    let mut environment = BTreeMap::new();
    let args = if operation == Operation::Handoff {
        if !target.handoff {
            return Err("Target adapter does not support context handoff".into());
        }
        let context = Path::new(&checkpoint.context_file);
        check_path(context_root)?;
        check_path(context)?;
        if !context.starts_with(context_root)
            || context == context_root
            || read_private(context)?.is_none()
        {
            return Err("Checkpoint context is missing or outside its private directory".into());
        }
        vec![format!(
            "Read the local handoff at {}. It is a terminal excerpt, not full session history. Review it and ask what to continue.",
            context.display()
        )]
    } else {
        if checkpoint.harness != target.harness || !valid_sid(&checkpoint.sid) {
            return Err("Native resume/fork requires an ID from the same harness".into());
        }
        if operation == Operation::Resume && !target.resume
            || operation == Operation::NativeFork && !target.native_fork
        {
            return Err("Installed harness does not advertise this capability".into());
        }
        if !checkpoint.profile.is_empty() {
            if !Path::new(&checkpoint.profile).is_dir() {
                return Err("Saved account profile is unavailable".into());
            }
            let key = match target.harness.as_str() {
                "claude" => "CLAUDE_CONFIG_DIR",
                "codex" => "CODEX_HOME",
                "grok" => "GROK_HOME",
                "pi" => "PI_CODING_AGENT_DIR",
                _ => return Err("No profile adapter for this harness".into()),
            };
            environment.insert(key.into(), checkpoint.profile.clone());
        }
        match target.harness.as_str() {
            "pi" => vec![
                if operation == Operation::NativeFork {
                    "--fork"
                } else {
                    "--session"
                }
                .into(),
                checkpoint.sid.clone(),
            ],
            "codex" => vec![
                if operation == Operation::NativeFork {
                    "fork"
                } else {
                    "resume"
                }
                .into(),
                checkpoint.sid.clone(),
            ],
            "claude" | "grok" => {
                let mut args = vec!["--resume".into(), checkpoint.sid.clone()];
                if operation == Operation::NativeFork {
                    args.push("--fork-session".into());
                }
                args
            }
            _ => return Err("Native session adapter is not implemented for this harness".into()),
        }
    };
    Ok(LaunchPlan {
        binary: target.binary.clone(),
        args,
        environment,
        cwd: checkpoint.cwd.clone().into(),
        operation,
    })
}
/// View duplication shares a terminal; it never clones a native conversation.
#[derive(Debug, Clone)]
pub struct DuplicateViewPlan {
    pub source: TerminalRef,
    pub destination: TerminalRef,
    pub read_only: bool,
}
impl DuplicateViewPlan {
    pub fn new(source: TerminalRef, destination: TerminalRef, read_only: bool) -> Result<Self> {
        source.validate()?;
        destination.validate()?;
        if source.same_identity(&destination) {
            return Err("Choose a different destination terminal".into());
        }
        Ok(Self {
            source,
            destination,
            read_only,
        })
    }
    pub fn revalidate(&self, source: &TerminalRef, destination: &TerminalRef) -> Result<()> {
        if !self.source.same_identity(source) || !self.destination.same_identity(destination) {
            return Err("View source or destination changed; refresh".into());
        }
        Ok(())
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn imports_legacy_and_canonical_registries() {
        let dir =
            std::env::temp_dir().join(format!("daedalus-checkpoint-test-{}", std::process::id()));
        // This directory belongs exclusively to this isolated test.
        let registry = Registry {
            checkpoints: vec![row()],
        };
        registry.save(&dir).unwrap();
        write_private(&dir.join("session_checkpoints.json"),
            &serde_json::to_vec(&serde_json::json!({"seats": {"Old session": [{"session_id": "old-id", "cwd": std::env::temp_dir(), "claude_config_dir": crate::test_support::path("/tmp/profile")}]}})).unwrap()).unwrap();
        let imported = Registry::load(&dir).unwrap();
        assert_eq!(imported.checkpoints.len(), 2);
        assert_eq!(imported.checkpoints[1].sid, "old-id");
        assert_eq!(
            imported.checkpoints[1].profile,
            crate::test_support::path("/tmp/profile")
        );
        write_private(
            &dir.join("session-checkpoints.json"),
            &serde_json::to_vec(&registry).unwrap(),
        )
        .unwrap();
        imported.save(&dir).unwrap();
        assert_eq!(Registry::load(&dir).unwrap().checkpoints.len(), 2);
        std::fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn capture_rejects_changed_process() {
        let before = TerminalRef {
            backend: super::super::layouts::Backend::Herdr,
            server: "server".into(),
            pane_id: "pane".into(),
            terminal_id: "terminal".into(),
            generation: "1".into(),
            label: String::new(),
            place: String::new(),
        };
        let mut after = before.clone();
        after.generation = "2".into();
        assert!(
            Registry::default()
                .add_verified(row(), &before, &after)
                .is_err()
        );
    }
    fn row() -> Checkpoint {
        Checkpoint {
            name: "Saved".into(),
            harness: "claude".into(),
            sid: "session-123".into(),
            cwd: std::env::temp_dir().to_string_lossy().into(),
            profile: String::new(),
            origin: "User supplied ID".into(),
            created_at: String::new(),
            context_file: String::new(),
            source: None,
        }
    }
    fn caps() -> Capabilities {
        Capabilities {
            harness: "claude".into(),
            binary: crate::test_support::path("/usr/bin/claude").into(),
            resume: true,
            native_fork: true,
            handoff: true,
        }
    }
    #[test]
    fn fork_parent_id_not_saved() {
        let codex = ["codex", "fork", "parent"].map(String::from);
        assert_eq!(resolve_sid(&codex, "codex", "parent").unwrap(), None);
        let args = ["claude", "--resume", "parent", "--fork-session"].map(String::from);
        assert_eq!(resolve_sid(&args, "claude", "parent").unwrap(), None);
        assert_eq!(
            resolve_sid(&args, "claude", "child").unwrap(),
            Some("child".into())
        );
    }
    #[test]
    fn codex_resume_options_do_not_hide_or_impersonate_session_id() {
        let args = [
            "codex",
            "resume",
            "--no-daemon",
            "-m",
            "model-name",
            "-c",
            "key=value",
            "session-123",
        ]
        .map(String::from);
        assert_eq!(
            resolve_sid(&args, "codex", "").unwrap(),
            Some("session-123".into())
        );
        assert!(resolve_sid(&args, "codex", "different").is_err());
        let args = ["codex", "resume", "--last", "prompt"].map(String::from);
        assert_eq!(resolve_sid(&args, "codex", "").unwrap(), None);
        let args = ["codex", "resume", "--unrecognized", "value"].map(String::from);
        assert_eq!(resolve_sid(&args, "codex", "").unwrap(), None);
        let args = ["codex", "fork", "-m", "model", "parent"].map(String::from);
        assert_eq!(resolve_sid(&args, "codex", "parent").unwrap(), None);
        assert_eq!(
            resolve_sid(&args, "codex", "child").unwrap(),
            Some("child".into())
        );
    }
    #[test]
    fn conflicting_identity_rejected() {
        let args = ["claude", "--resume", "one"].map(String::from);
        assert!(resolve_sid(&args, "claude", "two").is_err());
    }
    #[test]
    fn native_fork_requires_capability_and_same_harness() {
        let mut target = caps();
        target.native_fork = false;
        assert!(
            plan(
                &row(),
                Operation::NativeFork,
                &target,
                Path::new(crate::test_support::path("/tmp/checkpoints"))
            )
            .is_err()
        );
        target.native_fork = true;
        target.harness = "grok".into();
        assert!(
            plan(
                &row(),
                Operation::NativeFork,
                &target,
                Path::new(crate::test_support::path("/tmp/checkpoints"))
            )
            .is_err()
        );
    }
    #[test]
    fn launch_is_argument_vector_not_shell() {
        let launch = plan(
            &row(),
            Operation::NativeFork,
            &caps(),
            Path::new(crate::test_support::path("/tmp/checkpoints")),
        )
        .unwrap();
        assert_eq!(launch.args, ["--resume", "session-123", "--fork-session"]);
        let mut bad = row();
        bad.sid = "$(touch /tmp/bad)".into();
        assert!(bad.validate().is_err());
    }
    #[test]
    fn context_outside_root_rejected() {
        let mut checkpoint = row();
        checkpoint.context_file = "/etc/passwd".into();
        assert!(
            plan(
                &checkpoint,
                Operation::Handoff,
                &caps(),
                Path::new(crate::test_support::path("/tmp/checkpoints"))
            )
            .is_err()
        );
    }
    #[test]
    fn pi_native_resume_and_fork_do_not_create_a_replacement_id() {
        let mut checkpoint = row();
        checkpoint.harness = "pi".into();
        let mut capabilities = caps();
        capabilities.harness = "pi".into();
        capabilities.binary = crate::test_support::path("/fixture/pi").into();
        for (operation, flag) in [
            (Operation::Resume, "--session"),
            (Operation::NativeFork, "--fork"),
        ] {
            let planned = plan(
                &checkpoint,
                operation,
                &capabilities,
                Path::new(crate::test_support::path("/unused")),
            )
            .unwrap();
            assert_eq!(planned.args, [flag, &checkpoint.sid]);
            assert!(!planned.args.contains(&"--session-id".to_owned()));
        }
    }
}
