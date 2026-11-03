//! Session references and launch plans. No process execution or credential copying.
use super::checkpoint_budget::Guard;
use super::layouts::{Result, TerminalRef, check_path, read_private};
#[cfg(test)]
use super::{checkpoint_budget::Policy, layouts::write_private};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
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
    #[cfg(test)]
    pub fn load(directory: &Path) -> Result<Self> {
        Self::load_with(directory, &mut Guard::new(Policy::default())?)
    }
    pub fn load_with(directory: &Path, guard: &mut Guard<'_>) -> Result<Self> {
        Self::load_observed(directory, guard, |_, _| Ok(()))
    }
    pub(crate) fn load_observed(
        directory: &Path,
        guard: &mut Guard<'_>,
        mut observe: impl FnMut(&Path, &[u8]) -> Result<()>,
    ) -> Result<Self> {
        let mut checkpoints = Vec::new();
        for name in [
            "dashboard-checkpoints.json",
            "session-checkpoints.json",
            "session_checkpoints.json",
        ] {
            let path = directory.join(name);
            let Some(bytes) = guard.read(&path)? else {
                continue;
            };
            observe(&path, &bytes)?;
            guard.time_left()?;
            let mut value: serde_json::Value =
                serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
            if !value.is_object() {
                return Err("Invalid checkpoint registry object".into());
            }
            if name == "session_checkpoints.json" {
                let serde_json::Value::Object(seats) = value["seats"].take() else {
                    return Err("Legacy checkpoint seats must be an object".into());
                };
                for (seat, entries) in seats {
                    let serde_json::Value::Array(entries) = entries else {
                        return Err("Legacy checkpoint seat must be an array".into());
                    };
                    guard.entries(entries.len())?;
                    for entry in entries {
                        guard.time_left()?;
                        let serde_json::Value::Object(mut entry) = entry else {
                            return Err("Invalid checkpoint".into());
                        };
                        if entry.get("name").is_some_and(|v| v != &seat)
                            || entry.get("harness").is_some_and(|v| v != "claude")
                        {
                            return Err("Conflicting legacy checkpoint identity".into());
                        }
                        entry.insert("name".into(), seat.clone().into());
                        entry.insert("harness".into(), "claude".into());
                        checkpoints.push(decode_legacy(serde_json::Value::Object(entry))?);
                    }
                }
            } else {
                let serde_json::Value::Array(entries) = value["checkpoints"].take() else {
                    return Err("Checkpoint registry must contain an array".into());
                };
                guard.entries(entries.len())?;
                for entry in entries {
                    guard.time_left()?;
                    checkpoints.push(decode_legacy(entry)?);
                }
            }
        }
        // Reading old registries remains safe after the merged canonical file
        // has been saved: only byte-equivalent normalized records collapse.
        let mut seen = BTreeSet::new();
        let mut unique = Vec::new();
        for row in checkpoints {
            if seen.insert(guard.encode(&row)?) {
                unique.push(row);
            }
        }
        Ok(Self {
            checkpoints: unique,
        })
    }
    /// Saves the canonical registry. Legacy records are deduplicated on reload;
    /// importers can archive the originals after their backup is verified.
    #[cfg(test)]
    pub fn save(&self, directory: &Path) -> Result<()> {
        self.save_with(directory, &mut Guard::new(Policy::default())?)
    }
    pub fn save_with(&self, directory: &Path, guard: &mut Guard<'_>) -> Result<()> {
        if self.checkpoints.len() as u64 > guard.policy.entries {
            return Err("Checkpoint entry limit reached; review checkpoint_entry_limit".into());
        }
        for checkpoint in &self.checkpoints {
            guard.time_left()?;
            checkpoint.validate()?;
        }
        let bytes = guard.encode(self)?;
        guard.time_left()?;
        super::layouts::write_private_limit(
            &directory.join("dashboard-checkpoints.json"),
            &bytes,
            guard.policy.file_bytes,
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
    let mut profile = None;
    for key in ["profile", "claude_config_dir", "grok_home"] {
        if let Some(value) = row.get(key) {
            let value = value.as_str().ok_or("Invalid checkpoint profile")?;
            if profile.is_some_and(|old| old != value) {
                return Err("Conflicting checkpoint profile aliases".into());
            }
            profile = Some(value);
        }
    }
    let profile = profile.unwrap_or("").to_owned();
    row.insert("profile".into(), profile.into());
    let checkpoint: Checkpoint = serde_json::from_value(value).map_err(|e| e.to_string())?;
    checkpoint.validate()?;
    Ok(checkpoint)
}

pub fn key(checkpoint: &Checkpoint) -> Result<String> {
    let encoded = serde_json::to_vec(checkpoint).map_err(|_| "Cannot encode checkpoint")?;
    Ok(format!("checkpoint:{:x}", Sha256::digest(encoded)))
}
pub fn removals(directory: &Path, guard: &mut Guard<'_>) -> Result<BTreeSet<String>> {
    removals_observed(directory, guard, |_, _| Ok(()))
}
pub(crate) fn removals_observed(
    directory: &Path,
    guard: &mut Guard<'_>,
    mut observe: impl FnMut(&Path, &[u8]) -> Result<()>,
) -> Result<BTreeSet<String>> {
    let path = directory.join("checkpoint-removals.json");
    let Some(bytes) = guard.read(&path)? else {
        return Ok(BTreeSet::new());
    };
    observe(&path, &bytes)?;
    let rows: Vec<String> =
        serde_json::from_slice(&bytes).map_err(|_| "Invalid checkpoint removal index")?;
    guard.entries(rows.len())?;
    Ok(rows.into_iter().collect())
}

// Resume options may precede the positional session ID. Do not mistake a
// model/profile value or an initial prompt selected with --last for that ID.
struct CodexLaunch<'a> {
    sid: Option<&'a str>,
    forking: bool,
}
fn canonical_uuid(value: &str) -> bool {
    value.len() == 36
        && value.bytes().enumerate().all(|(index, byte)| {
            if matches!(index, 8 | 13 | 18 | 23) {
                byte == b'-'
            } else {
                byte.is_ascii_hexdigit()
            }
        })
}
/// Interpret only the known interactive CLI prefix. A resume name is a lookup
/// selector, not a conversation ID. Unknown option arity cannot prove a binding.
fn codex_launch(argv: &[String]) -> Option<CodexLaunch<'_>> {
    let mut args = argv.iter().skip(1);
    let mut mode = None;
    while let Some(arg) = args.next() {
        if arg == "--" {
            return Some(CodexLaunch {
                sid: args
                    .next()
                    .filter(|value| mode.is_some() && canonical_uuid(value))
                    .map(String::as_str),
                forking: mode == Some(true),
            });
        }
        let (flag, inline) = if arg.starts_with("--") {
            arg.split_once('=')
                .map_or((arg.as_str(), None), |(flag, value)| (flag, Some(value)))
        } else if arg.len() > 2
            && matches!(arg.get(..2), Some("-c" | "-m" | "-p" | "-s" | "-a" | "-C"))
        {
            (&arg[..2], Some(&arg[2..]))
        } else {
            (arg.as_str(), None)
        };
        match flag {
            "--last" if mode.is_some() && inline.is_none() => {
                return Some(CodexLaunch {
                    sid: None,
                    forking: mode == Some(true),
                });
            }
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
                let value = inline.or_else(|| args.next().map(String::as_str))?;
                if value.is_empty() {
                    return None;
                }
            }
            "--no-daemon"
            | "--all"
            | "--include-non-interactive"
            | "--strict-config"
            | "--oss"
            | "--approve-for-me"
            | "--dangerously-bypass-approvals-and-sandbox"
            | "--dangerously-bypass-hook-trust"
            | "--full-auto"
            | "--worktree"
            | "--search"
            | "--no-alt-screen"
                if inline.is_none() => {}
            value if value.starts_with('-') => {
                // The already identified resume command cannot become a fork.
                // Keep native evidence usable without guessing this option's value.
                return (mode == Some(false)).then_some(CodexLaunch {
                    sid: None,
                    forking: false,
                });
            }
            "resume" | "fork" if mode.is_none() => mode = Some(flag == "fork"),
            value => {
                return Some(CodexLaunch {
                    sid: (mode.is_some() && canonical_uuid(value)).then_some(value),
                    forking: mode == Some(true),
                });
            }
        }
    }
    Some(CodexLaunch {
        sid: None,
        forking: mode == Some(true),
    })
}

/// A fork parent's launch ID is never reported as the new child's ID.
pub fn resolve_sid(argv: &[String], harness: &str, reported: &str) -> Result<Option<String>> {
    if harness == "codex" {
        if !reported.is_empty() && !valid_sid(reported) {
            return Err("Invalid reported session ID".into());
        }
        let Some(launch) = codex_launch(argv) else {
            return Ok(None);
        };
        if launch.forking {
            return Ok(launch.sid.and_then(|parent| {
                (!reported.is_empty() && !parent.eq_ignore_ascii_case(reported))
                    .then(|| reported.to_owned())
            }));
        }
        if let Some(id) = launch.sid {
            if reported.is_empty() {
                return Ok(Some(id.to_ascii_lowercase()));
            }
            if !id.eq_ignore_ascii_case(reported) {
                return Err(
                    "Native metadata and launch arguments disagree on the session ID".into(),
                );
            }
        }
        return Ok((!reported.is_empty()).then(|| reported.to_owned()));
    }
    if matches!(harness, "claude" | "grok" | "opencode") {
        if !reported.is_empty() && !valid_sid(reported) {
            return Err("Invalid reported session ID".into());
        }
        let Some(launch) = super::session_args::parse(argv, harness)? else {
            return Ok(None);
        };
        let resume = launch.resume.filter(|value| {
            if harness == "opencode" {
                value.starts_with("ses_") && valid_sid(value)
            } else {
                canonical_uuid(value)
            }
        });
        let same = |left: &str, right: &str| {
            if harness == "opencode" {
                left == right
            } else {
                left.eq_ignore_ascii_case(right)
            }
        };
        if launch.forking {
            // A title, picker or continue flag does not establish the parent.
            // An additional creation ID cannot silently redefine that parent.
            if launch.created.is_some() {
                return Err("Fork launch combines parent and new-session selectors".into());
            }
            return Ok(resume.and_then(|parent| {
                (!reported.is_empty() && !same(parent, reported)).then(|| reported.to_owned())
            }));
        }
        let created = launch
            .created
            .map(|id| {
                if canonical_uuid(id) {
                    Ok(id)
                } else {
                    Err("Invalid launch session UUID".to_string())
                }
            })
            .transpose()?;
        if let (Some(resume), Some(created)) = (resume, created)
            && !same(resume, created)
        {
            return Err("Conflicting launch session selectors".into());
        }
        // OpenCode can switch conversations without restarting its UI. Its
        // current native selection supersedes a nonforking launch selector.
        if !reported.is_empty() {
            if harness != "opencode" && resume.or(created).is_some_and(|id| !same(id, reported)) {
                return Err(
                    "Native metadata and launch arguments disagree on the session ID".into(),
                );
            }
            return Ok(Some(reported.into()));
        }
        return Ok(resume.or(created).map(|id| {
            if harness == "opencode" {
                id.to_owned()
            } else {
                id.to_ascii_lowercase()
            }
        }));
    }
    let end = argv
        .iter()
        .position(|arg| arg == "--")
        .unwrap_or(argv.len());
    let argv = &argv[..end];
    let forking = argv.iter().any(|a| a == "--fork-session");
    let mut ids = Vec::new();
    for pair in argv.windows(2) {
        if matches!(pair[0].as_str(), "--resume" | "-r" | "--session-id") {
            ids.push(pair[1].as_str());
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
    struct Private(PathBuf);
    impl Private {
        fn new() -> Self {
            use std::sync::atomic::{AtomicU64, Ordering};
            static NEXT: AtomicU64 = AtomicU64::new(0);
            Self(std::env::temp_dir().join(format!(
                "daedalus-checkpoint-policy-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            )))
        }
    }
    impl Drop for Private {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    #[test]
    fn checkpoint_loaders_reject_duplicate_keys_conflicting_aliases_and_invalid_roots() {
        let dir = Private::new();
        let canonical = serde_json::to_string(&Registry {
            checkpoints: vec![row()],
        })
        .unwrap();
        let profile = serde_json::to_string(&row().profile).unwrap();
        let duplicate = canonical.replace(
            &format!("\"profile\":{profile}"),
            &format!(
                "\"profile\":{profile},\"profile\":{}",
                serde_json::to_string(crate::test_support::path("/other/profile")).unwrap()
            ),
        );
        for name in [
            "dashboard-checkpoints.json",
            "session-checkpoints.json",
            "session_checkpoints.json",
        ] {
            let raw = if name == "session_checkpoints.json" {
                r#"{"seats":{"Repeated":[],"Repeated":[]}}"#.to_owned()
            } else {
                duplicate.clone()
            };
            write_private(&dir.0.join(name), raw.as_bytes()).unwrap();
            assert!(Registry::load(&dir.0).unwrap_err().contains("ambiguous"));
            assert_eq!(std::fs::read(dir.0.join(name)).unwrap(), raw.as_bytes());
            std::fs::remove_file(dir.0.join(name)).unwrap();
        }
        for raw in [
            "[]".to_owned(),
            "null".into(),
            canonical.replace(
                &format!("\"profile\":{profile}"),
                &format!(
                    "\"profile\":{profile},\"grok_home\":{}",
                    serde_json::to_string(crate::test_support::path("/other/profile")).unwrap()
                ),
            ),
        ] {
            write_private(&dir.0.join("dashboard-checkpoints.json"), raw.as_bytes()).unwrap();
            assert!(Registry::load(&dir.0).is_err());
        }
        write_private(
            &dir.0.join("dashboard-checkpoints.json"),
            canonical.as_bytes(),
        )
        .unwrap();
        assert_eq!(Registry::load(&dir.0).unwrap().checkpoints.len(), 1);
    }
    #[test]
    fn checkpoint_policy_covers_legacy_records_removals_values_bytes_and_deadline() {
        let dir = Private::new();
        let mut second = row();
        second.name = "second".into();
        Registry {
            checkpoints: vec![row(), second],
        }
        .save(&dir.0)
        .unwrap();
        let original = std::fs::read(dir.0.join("dashboard-checkpoints.json")).unwrap();
        let mut policy = Policy {
            entries: 1,
            ..Policy::default()
        };
        assert!(
            Registry::load_with(&dir.0, &mut Guard::new(policy).unwrap())
                .unwrap_err()
                .contains("entry limit")
        );
        policy.entries = 2;
        write_private(
            &dir.0.join("checkpoint-removals.json"),
            b"[\"retained-unknown-key\"]",
        )
        .unwrap();
        let mut guard = Guard::new(policy).unwrap();
        assert_eq!(removals(&dir.0, &mut guard).unwrap().len(), 1);
        assert!(Registry::load_with(&dir.0, &mut guard).is_err());
        let mut value: serde_json::Value = serde_json::from_slice(&original).unwrap();
        value["extra"] = vec![serde_json::Value::Null; 200].into();
        write_private(
            &dir.0.join("dashboard-checkpoints.json"),
            &serde_json::to_vec(&value).unwrap(),
        )
        .unwrap();
        policy = Policy {
            values: 128,
            ..Policy::default()
        };
        assert!(
            Registry::load_with(&dir.0, &mut Guard::new(policy).unwrap())
                .unwrap_err()
                .contains("JSON value limit")
        );
        value["extra"] = "x".repeat(5000).into();
        let oversized = serde_json::to_vec(&value).unwrap();
        write_private(&dir.0.join("dashboard-checkpoints.json"), &oversized).unwrap();
        policy = Policy {
            file_bytes: 4096,
            ..Policy::default()
        };
        assert!(
            Registry::load_with(&dir.0, &mut Guard::new(policy).unwrap())
                .unwrap_err()
                .contains("file limit")
        );
        policy = Policy {
            work_bytes: 4096,
            ..Policy::default()
        };
        assert!(
            Registry::load_with(&dir.0, &mut Guard::new(policy).unwrap())
                .unwrap_err()
                .contains("byte-work limit")
        );
        let mut guard = Guard::new(Policy::default()).unwrap();
        guard.constrain_deadline(std::time::Instant::now() - std::time::Duration::from_millis(1));
        assert!(
            Registry::load_with(&dir.0, &mut guard)
                .unwrap_err()
                .contains("deadline")
        );
        assert_eq!(
            std::fs::read(dir.0.join("dashboard-checkpoints.json")).unwrap(),
            oversized
        );
    }
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
        let parent = "019a043e-1234-7000-8000-000000000001";
        let codex = ["codex", "fork", parent].map(String::from);
        assert_eq!(resolve_sid(&codex, "codex", parent).unwrap(), None);
        let args = ["claude", "--resume", parent, "--fork-session"].map(String::from);
        assert_eq!(resolve_sid(&args, "claude", parent).unwrap(), None);
        assert_eq!(
            resolve_sid(&args, "claude", "child").unwrap(),
            Some("child".into())
        );
    }
    #[test]
    fn opencode_resume_flags_and_current_native_selection_are_distinct() {
        for flag in ["--session", "-s"] {
            let args = ["opencode", flag, "ses_original"].map(String::from);
            assert_eq!(
                resolve_sid(&args, "opencode", "").unwrap(),
                Some("ses_original".into())
            );
            assert_eq!(
                resolve_sid(&args, "opencode", "ses_selected").unwrap(),
                Some("ses_selected".into())
            );
            assert_eq!(resolve_sid(&args, "claude", "").unwrap(), None);
        }
        assert_eq!(
            resolve_sid(&["opencode".into()], "opencode", "").unwrap(),
            None
        );
        assert!(resolve_sid(&[], "opencode", "../invalid").is_err());
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
            "019a043e-1234-7000-8000-000000000001",
        ]
        .map(String::from);
        assert_eq!(
            resolve_sid(&args, "codex", "").unwrap(),
            Some("019a043e-1234-7000-8000-000000000001".into())
        );
        assert!(resolve_sid(&args, "codex", "different").is_err());
        let args = ["codex", "resume", "--last", "prompt"].map(String::from);
        assert_eq!(resolve_sid(&args, "codex", "").unwrap(), None);
        let args = ["codex", "resume", "--unrecognized", "value"].map(String::from);
        assert_eq!(resolve_sid(&args, "codex", "").unwrap(), None);
        let args = [
            "codex",
            "fork",
            "-m",
            "model",
            "019a043e-1234-7000-8000-000000000001",
        ]
        .map(String::from);
        assert_eq!(
            resolve_sid(&args, "codex", "019a043e-1234-7000-8000-000000000001").unwrap(),
            None
        );
        assert_eq!(
            resolve_sid(&args, "codex", "child").unwrap(),
            Some("child".into())
        );
    }
    #[test]
    fn conflicting_identity_rejected() {
        let args = ["claude", "--resume", "019a043e-1234-7000-8000-000000000001"].map(String::from);
        assert!(resolve_sid(&args, "claude", "019a043e-1234-7000-8000-000000000002").is_err());
    }
    #[test]
    fn named_claude_and_grok_resumes_are_not_conversation_ids() {
        let id = "019a043e-1234-7000-8000-000000000001";
        for harness in ["claude", "grok"] {
            for flag in ["--resume", "-r"] {
                let args = [harness, flag, "work"].map(String::from);
                assert_eq!(resolve_sid(&args, harness, "").unwrap(), None);
                assert_eq!(
                    resolve_sid(&args, harness, id).unwrap().as_deref(),
                    Some(id)
                );
            }
        }
    }
    #[test]
    fn native_selector_forms_follow_each_provider_instead_of_shared_flag_names() {
        let id = "019a043e-1234-7000-8000-000000000001";
        for (harness, selector) in [
            ("claude", format!("--resume={id}")),
            ("claude", format!("--session-id={id}")),
            ("claude", format!("-r{id}")),
            ("grok", format!("--resume={id}")),
            ("grok", format!("-r{id}")),
            ("grok", format!("-s{id}")),
            ("grok", format!("-s={id}")),
            ("grok", format!("-r={id}")),
            ("opencode", "--session=ses_selected".into()),
            ("opencode", "-sses_selected".into()),
            ("opencode", "-s=ses_selected".into()),
        ] {
            let expected = if harness == "opencode" {
                "ses_selected"
            } else {
                id
            };
            let args = [harness.to_owned(), selector];
            assert_eq!(
                resolve_sid(&args, harness, "").unwrap().as_deref(),
                Some(expected)
            );
        }
        let args = ["opencode", "--resume", id].map(String::from);
        assert_eq!(resolve_sid(&args, "opencode", "").unwrap(), None);
    }
    #[test]
    fn provider_arity_and_ambiguity_do_not_override_verified_conversation_evidence() {
        let id = "019a043e-1234-7000-8000-000000000001";
        for (harness, options) in [
            (
                "claude",
                vec!["--allowed-tools", "Bash", "Edit", "--resume", id],
            ),
            ("claude", vec!["--resume", "--model", "example"]),
            ("claude", vec!["--tools=", "--resume", id]),
            ("claude", vec!["--append-system-prompt", "", "--resume", id]),
            (
                "grok",
                vec!["--system-prompt=--fork-session", "--resume", id],
            ),
            (
                "opencode",
                vec!["--prompt=--fork", "--session", "ses_original"],
            ),
        ] {
            let args: Vec<String> = [harness]
                .into_iter()
                .chain(options)
                .map(String::from)
                .collect();
            let expected = if harness == "opencode" {
                "ses_selected"
            } else {
                id
            };
            assert_eq!(
                resolve_sid(&args, harness, expected).unwrap().as_deref(),
                Some(expected)
            );
        }
        for harness in ["claude", "grok", "opencode"] {
            let args = [harness, "--future-option", "value"].map(String::from);
            assert_eq!(resolve_sid(&args, harness, id).unwrap(), None);
            assert!(resolve_sid(&[], harness, "../invalid").is_err());
        }
        for harness in ["claude", "grok"] {
            let args = [
                harness,
                "--resume",
                id,
                "--session-id",
                "019a043e-1234-7000-8000-000000000002",
            ]
            .map(String::from);
            assert!(resolve_sid(&args, harness, "").is_err());
            assert!(resolve_sid(&args, harness, id).is_err());
            let args = [harness, "--resume", id, "--continue"].map(String::from);
            assert!(resolve_sid(&args, harness, id).is_err());
            let args = [
                harness,
                "--resume",
                id,
                "--resume",
                "019a043e-1234-7000-8000-000000000002",
            ]
            .map(String::from);
            assert!(resolve_sid(&args, harness, id).is_err());
            let args = [harness, "--model"].map(String::from);
            assert!(resolve_sid(&args, harness, id).is_err());
            let args = [
                harness.to_owned(),
                "--resume".into(),
                id.to_ascii_uppercase(),
            ];
            assert_eq!(
                resolve_sid(&args, harness, id).unwrap().as_deref(),
                Some(id)
            );
        }
        assert!(resolve_sid(&["claude".into(), "--tools".into()], "claude", id).is_err());
    }
    #[test]
    fn native_forks_need_a_known_parent_and_distinct_reported_child() {
        let parent = "019a043e-1234-7000-8000-000000000001";
        let child = "019a043e-1234-7000-8000-000000000002";
        for harness in ["claude", "grok"] {
            let args = [harness, "--resume", parent, "--fork-session"].map(String::from);
            assert_eq!(resolve_sid(&args, harness, "").unwrap(), None);
            assert_eq!(resolve_sid(&args, harness, parent).unwrap(), None);
            assert_eq!(
                resolve_sid(&args, harness, child).unwrap().as_deref(),
                Some(child)
            );
            for prefix in [vec!["--continue"], vec!["--resume", "work"], vec![]] {
                let args: Vec<String> = [harness]
                    .into_iter()
                    .chain(prefix)
                    .chain(["--fork-session"])
                    .map(String::from)
                    .collect();
                assert_eq!(resolve_sid(&args, harness, parent).unwrap(), None);
            }
        }
    }
    #[test]
    fn opencode_fork_does_not_identify_the_parent_as_its_new_conversation() {
        for flag in ["--session", "-s"] {
            let args = ["opencode", flag, "ses_parent", "--fork"].map(String::from);
            assert_eq!(resolve_sid(&args, "opencode", "").unwrap(), None);
            assert_eq!(resolve_sid(&args, "opencode", "ses_parent").unwrap(), None);
            assert_eq!(
                resolve_sid(&args, "opencode", "ses_child")
                    .unwrap()
                    .as_deref(),
                Some("ses_child")
            );
            let no_fork = ["opencode", flag, "ses_parent", "--fork=false"].map(String::from);
            assert_eq!(
                resolve_sid(&no_fork, "opencode", "").unwrap().as_deref(),
                Some("ses_parent")
            );
            assert_eq!(
                resolve_sid(&no_fork, "opencode", "ses_selected")
                    .unwrap()
                    .as_deref(),
                Some("ses_selected")
            );
        }
        let args = [
            "opencode",
            "attach",
            "https://fixture.invalid",
            "--continue",
            "--fork",
        ]
        .map(String::from);
        assert_eq!(resolve_sid(&args, "opencode", "ses_parent").unwrap(), None);
        let args = ["opencode", "--session=ses_Parent", "--fork=true"].map(String::from);
        assert_eq!(
            resolve_sid(&args, "opencode", "ses_parent")
                .unwrap()
                .as_deref(),
            Some("ses_parent")
        );
        let args = ["opencode", "--session=ses_parent", "--no-fork"].map(String::from);
        assert_eq!(
            resolve_sid(&args, "opencode", "").unwrap().as_deref(),
            Some("ses_parent")
        );
        let args = [
            "opencode",
            "--session=ses_parent",
            "--fork=true",
            "--fork=false",
        ]
        .map(String::from);
        assert!(resolve_sid(&args, "opencode", "ses_parent").is_err());
    }
    #[test]
    fn codex_launch_identity_honors_global_options_and_inline_values() {
        let parent = "019a043e-1234-7000-8000-000000000001";
        let child = "019a043e-1234-7000-8000-000000000002";
        for options in [
            vec!["-c", "model=example", "--no-daemon"],
            vec!["--config=model=example", "--model=example"],
            vec!["-mmodel", "-pexample", "-C/project"],
        ] {
            for mode in ["resume", "fork"] {
                let args: Vec<String> = ["codex"]
                    .into_iter()
                    .chain(options.iter().copied())
                    .chain([mode, parent])
                    .map(String::from)
                    .collect();
                if mode == "fork" {
                    assert_eq!(resolve_sid(&args, "codex", parent).unwrap(), None);
                    assert_eq!(resolve_sid(&args, "codex", "").unwrap(), None);
                    assert_eq!(
                        resolve_sid(&args, "codex", child).unwrap().as_deref(),
                        Some(child)
                    );
                } else {
                    assert_eq!(
                        resolve_sid(&args, "codex", "").unwrap().as_deref(),
                        Some(parent)
                    );
                    assert!(resolve_sid(&args, "codex", child).is_err());
                }
            }
        }
        let args = ["codex", "resume", "--model=example", parent].map(String::from);
        assert_eq!(
            resolve_sid(&args, "codex", "").unwrap().as_deref(),
            Some(parent)
        );
    }
    #[test]
    fn literal_prompt_flags_never_select_or_fork_a_conversation() {
        let current = "019a043e-1234-7000-8000-000000000001";
        for harness in ["codex", "claude", "grok"] {
            let args = [harness, "--", "--fork-session"].map(String::from);
            assert_eq!(
                resolve_sid(&args, harness, current).unwrap().as_deref(),
                Some(current)
            );
        }
        // Codex accepts a name followed by a prompt after `--`. A name which
        // resembles a selector must not turn its prompt into a conversation ID.
        let args = ["codex", "resume", "--", "--resume", "example"].map(String::from);
        assert_eq!(resolve_sid(&args, "codex", "").unwrap(), None);
        assert_eq!(
            resolve_sid(&args, "codex", current).unwrap().as_deref(),
            Some(current)
        );
        let args = ["codex", "resume", current, "--", "--fork-session"].map(String::from);
        assert_eq!(
            resolve_sid(&args, "codex", current).unwrap().as_deref(),
            Some(current)
        );
    }
    #[test]
    fn codex_resume_names_need_reported_ids_and_unknown_options_stay_unresolved() {
        let current = "019a043e-1234-7000-8000-000000000001";
        let named = ["codex", "resume", "work"].map(String::from);
        assert_eq!(resolve_sid(&named, "codex", "").unwrap(), None);
        assert_eq!(
            resolve_sid(&named, "codex", current).unwrap().as_deref(),
            Some(current)
        );
        let unknown = ["codex", "resume", "--new-option", "value"].map(String::from);
        assert_eq!(resolve_sid(&unknown, "codex", "").unwrap(), None);
        assert_eq!(
            resolve_sid(&unknown, "codex", current).unwrap().as_deref(),
            Some(current)
        );
        for tail in [
            vec!["--new-option", "fork", current],
            vec!["fork", "--new-option", current],
            vec!["fork", "--last"],
            vec!["fork", "work"],
            vec!["--model"],
        ] {
            let args: Vec<String> = ["codex"]
                .into_iter()
                .chain(tail)
                .map(String::from)
                .collect();
            assert_eq!(resolve_sid(&args, "codex", current).unwrap(), None);
        }
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
