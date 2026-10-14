//! Reviewed first-launch in a newly created exact terminal. The child waits for
//! its returned terminal identity before registration and native provider exec.
use super::{
    config::ControlConfig,
    hook_launch,
    hook_runtime::{Options, Registration},
    hooks::{EventKind, Harness},
};
use crate::{
    adapters::{
        harnesses, process,
        terminal_actions::{Placement, Receipt, ReceiptState},
        terminals::{self, Terminals},
    },
    core::{
        checkpoints::{LaunchPlan, Operation},
        goal_store::Scope,
        layouts,
    },
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs,
    io::Read,
    os::unix::fs::DirBuilderExt,
    path::{Path, PathBuf},
    process::Command,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
type Result<T> = std::result::Result<T, String>;
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Request {
    pub harness: String,
    pub parent: Value,
    pub cwd: PathBuf,
    pub model: Option<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Plan {
    pub request: Request,
    pub configuration: ControlConfig,
    pub hook: hook_launch::Plan,
    environment: BTreeMap<String, String>,
    pub profile: PathBuf,
}
impl Plan {
    pub fn fingerprint(&self) -> Result<String> {
        Ok(format!(
            "{:x}",
            Sha256::digest(serde_json::to_vec(self).map_err(|_| "Cannot encode launch review")?)
        ))
    }
    pub fn review(&self) -> Value {
        json!({"harness":self.request.harness,"profile":self.profile,"cwd":self.request.cwd,"parent":{"backend":self.request.parent.get("backend"),"pane_id":self.request.parent.get("pane_id"),"session":self.request.parent.get("session"),"workspace":self.request.parent.get("workspace_name")},"conversation":self.hook.spec.registration.scope.conversation,"action":"Create a new split pane; register hooks before native provider starts","account":"Selected native profile; sign-in remains with the provider","delivery":"Delivery and progress hooks are preconfigured; connect an explicitly authorized Telegram recipient before messages can be sent. Memory stays optional.","native_hook_profile":self.hook.spec.hook_profile,"created_hook_file":self.hook.native_hook_file(),"capabilities":if self.request.harness == "grok" { "Native turn-end goal continuation; feeder readiness remains unavailable" } else { "Native lifecycle hooks" }})
    }
    fn ticket(&self) -> PathBuf {
        self.hook
            .spec
            .registration
            .directory
            .join(format!("launch-ticket-{}", self.hook.spec.registration.id))
    }
}
fn uuid() -> Result<String> {
    let mut bytes = [0u8; 16];
    fs::File::open("/dev/urandom")
        .and_then(|mut f| f.read_exact(&mut bytes))
        .map_err(|_| "Cannot allocate a session UUID")?;
    bytes[6] = (bytes[6] & 15) | 64;
    bytes[8] = (bytes[8] & 63) | 128;
    let s = bytes.iter().map(|b| format!("{b:02x}")).collect::<String>();
    Ok(format!(
        "{}-{}-{}-{}-{}",
        &s[..8],
        &s[8..12],
        &s[12..16],
        &s[16..20],
        &s[20..]
    ))
}
fn resolve_node(env: &BTreeMap<String, String>) -> Result<PathBuf> {
    env.get("PATH")
        .into_iter()
        .flat_map(|p| std::env::split_paths(p))
        .map(|p| p.join("node"))
        .find_map(|p| fs::canonicalize(p).ok())
        .ok_or("Node is required by the selected Pi installation".into())
}
pub fn prepare(
    config: &ControlConfig,
    directory: &Path,
    terminals: &Terminals,
    environment: &BTreeMap<String, String>,
    request: Request,
    now: u64,
) -> Result<Plan> {
    config.validate()?;
    let parent = terminals.verify(&request.parent)?;
    let identity = terminals::identity(&parent)?;
    if !matches!(request.harness.as_str(), "claude" | "pi" | "grok") {
        return Err("Guided hooked launch currently supports Claude, Pi and Grok".into());
    }
    layouts::check_path(&request.cwd)?;
    if !request.cwd.is_dir() {
        return Err("Project directory is unavailable".into());
    }
    let tool = harnesses::tool(&request.harness).ok_or("Unknown harness")?;
    let binary = fs::canonicalize(
        harnesses::executable(&tool, environment).ok_or("Install this harness first")?,
    )
    .map_err(|_| "Cannot resolve native harness")?;
    let native =
        harnesses::launch_plan(&tool, &binary, &request.cwd, environment, &config.profiles)?;
    let profile = harnesses::profile_directory(&tool, environment, &config.profiles)?;
    let (program, entrypoint) = if request.harness == "pi" {
        (resolve_node(environment)?, Some(binary))
    } else {
        (binary, None)
    };
    hook_launch::validate_executable(&program, true)?;
    if let Some(path) = &entrypoint {
        hook_launch::validate_executable(path, false)?;
    }
    let mut query = Command::new(&program);
    if let Some(path) = &entrypoint {
        query.arg(path);
    }
    query
        .arg("--version")
        .env_clear()
        .envs(&native.environment)
        .current_dir(&request.cwd);
    let output = process::run(&mut query, Duration::from_secs(10), 16384)
        .map_err(|_| "Cannot verify installed provider version")?;
    if !output.status.success() {
        return Err("Provider version query failed".into());
    }
    let text = String::from_utf8(output.stdout).map_err(|_| "Invalid version output")?;
    let version = text
        .split_whitespace()
        .map(|s| s.trim_start_matches('v'))
        .find(|s| {
            let p = s.split('.').collect::<Vec<_>>();
            p.len() == 3
                && p.iter()
                    .all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()))
        })
        .ok_or("Provider version is not recognized")?;
    let session = uuid()?;
    let id = format!("launch-{session}");
    let harness = if request.harness == "pi" {
        Harness::Pi
    } else if request.harness == "grok" {
        Harness::Grok
    } else {
        Harness::Claude
    };
    let events = if harness == Harness::Pi {
        vec![
            EventKind::SessionStart,
            EventKind::UserPromptSubmit,
            EventKind::Activity,
            EventKind::PostToolUse,
            EventKind::AssistantResponse,
            EventKind::Stop,
            EventKind::Settled,
            EventKind::SessionEnd,
        ]
    } else {
        vec![
            EventKind::SessionStart,
            EventKind::UserPromptSubmit,
            EventKind::PostToolUse,
            EventKind::Stop,
            EventKind::SessionEnd,
        ]
    };
    // Claude uses its documented cwd-encoded project transcript directory.
    let transcript = if harness == Harness::Claude {
        let cwd = request.cwd.to_str().ok_or("Project path is not UTF-8")?;
        if cwd.len() > 180
            || !cwd
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"/_-".contains(&b))
        {
            return Err("Guided Claude launch currently requires an ASCII project path up to 180 bytes using letters, digits, slash, underscore or hyphen; use the explicit launch CLI with the provider's transcript path for other projects".into());
        }
        let encoded = request
            .cwd
            .to_str()
            .ok_or("Project path is not UTF-8")?
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
            .collect::<String>();
        Some(
            profile
                .join("projects")
                .join(encoded)
                .join(format!("{session}.jsonl")),
        )
    } else {
        None
    };
    let spec = hook_launch::Spec {
        registration: Registration {
            version: 1,
            id,
            directory: directory.into(),
            scope: Scope {
                instance: config.agent_name.clone(),
                terminal: identity,
                harness,
                conversation: session,
            },
            goal_id: None,
            harness_process: None,
            server_emitter: None,
            transcript,
            events,
            options: Options::guided(),
            activity_lease_ms: 60_000,
            pending_timeout_ms: 30_000,
            hook_timeout_ms: 10_000,
        },
        native_binary: program,
        entrypoint,
        hook_profile: (harness == Harness::Grok).then(|| profile.clone()),
        daedalus_binary: fs::canonicalize(
            std::env::current_exe().map_err(|_| "Cannot locate Daedalus")?,
        )
        .map_err(|_| "Cannot resolve Daedalus")?,
        working_directory: request.cwd.clone(),
        observed_version: version.into(),
        expires_ms: now.saturating_add(600_000),
        model: request.model.clone(),
    };
    let hook = hook_launch::prepare(spec, now)?;
    let mut request = request;
    request.parent = parent;
    Ok(Plan {
        request,
        configuration: config.clone(),
        hook,
        environment: native.environment,
        profile,
    })
}
pub fn apply(
    plan: &Plan,
    config: &ControlConfig,
    terminals: &Terminals,
    approved: &str,
    now: u64,
) -> Result<Receipt> {
    if plan.fingerprint()? != approved || config != &plan.configuration {
        return Err("Launch review or configuration changed".into());
    }
    let current = ControlConfig::parse(
        &layouts::read_private(&plan.hook.spec.registration.directory.join("control.json"))?
            .ok_or("Current instance configuration is missing")?,
    )?;
    if current != plan.configuration {
        return Err("Instance configuration changed; review launch again".into());
    }
    terminals.verify(&plan.request.parent)?;
    if hook_launch::prepare(plan.hook.spec.clone(), now)?.fingerprint()?
        != plan.hook.fingerprint()?
    {
        return Err("Launch files changed; review again".into());
    }
    let directory = &plan.hook.spec.registration.directory;
    let _active = super::hook_runtime::active_guard(directory).map_err(|e| e.to_string())?;
    let ticket = plan.ticket();
    layouts::check_path(&ticket)?;
    fs::DirBuilder::new()
        .mode(0o700)
        .create(&ticket)
        .map_err(|_| "Launch already submitted or ticket unavailable")?;
    layouts::write_private(
        &ticket.join("expected.json"),
        &serde_json::to_vec(&plan.hook).map_err(|_| "Cannot record launch intent")?,
    )?;
    let launch = LaunchPlan {
        binary: plan.hook.spec.daedalus_binary.clone(),
        args: vec![
            "hook".into(),
            "await-launch".into(),
            "--from".into(),
            ticket.to_string_lossy().into_owned(),
        ],
        environment: plan.environment.clone(),
        cwd: plan.request.cwd.clone(),
        operation: Operation::Handoff,
    };
    let mut receipt = terminals.spawn(&plan.request.parent, &launch, Placement::Split)?;
    if receipt.state != ReceiptState::Submitted {
        return Ok(receipt);
    }
    let Some(created) = receipt.terminal.as_ref() else {
        return Ok(receipt);
    };
    let publication = (|| -> Result<()> {
        let actual = terminals.verify(created)?;
        let identity = terminals::identity(&actual)?;
        if identity == plan.hook.spec.registration.scope.terminal {
            return Err("Server returned the original terminal".into());
        }
        let mut spec = plan.hook.spec.clone();
        spec.registration.scope.terminal = identity;
        let bound = hook_launch::prepare(spec, now)?;
        let path = hook_launch::save(&bound, &bound.fingerprint()?, now)?;
        layouts::write_private(
            &ticket.join("ready.json"),
            &serde_json::to_vec(&path).map_err(|_| "Cannot publish launch identity")?,
        )?;
        Ok(())
    })();
    if publication.is_err() {
        receipt.state = ReceiptState::Unconfirmed;
        receipt.message =
            "New pane exists but hook binding was not confirmed; inspect before retrying".into();
        return Ok(receipt);
    }
    receipt.state = ReceiptState::Submitted;
    receipt.message="New pane created; hook bootstrap submitted. Provider authentication and readiness are not yet confirmed".into();
    Ok(receipt)
}
pub(crate) fn bound_plan(ticket: &Path) -> Result<Option<PathBuf>> {
    layouts::check_path(ticket)?;
    let expected: hook_launch::Plan = serde_json::from_slice(
        &layouts::read_private(&ticket.join("expected.json"))?.ok_or("Missing launch ticket")?,
    )
    .map_err(|_| "Invalid launch ticket")?;
    if ticket
        != expected
            .spec
            .registration
            .directory
            .join(format!("launch-ticket-{}", expected.spec.registration.id))
    {
        return Err("Launch ticket identity mismatch".into());
    }
    let Some(bytes) = layouts::read_private(&ticket.join("ready.json"))? else {
        return Ok(None);
    };
    let path: PathBuf = serde_json::from_slice(&bytes).map_err(|_| "Invalid launch publication")?;
    if path != expected.path() {
        return Err("Unexpected launch publication".into());
    }
    let mut actual: hook_launch::Plan =
        serde_json::from_slice(&layouts::read_private(&path)?.ok_or("Missing bound launch plan")?)
            .map_err(|_| "Invalid bound launch plan")?;
    if actual.spec.registration.scope.terminal == expected.spec.registration.scope.terminal {
        return Err("Launch was not bound to a new terminal".into());
    }
    actual.spec.registration.scope.terminal = expected.spec.registration.scope.terminal.clone();
    if actual.fingerprint()? != expected.fingerprint()? {
        return Err("Bound launch differs from reviewed provider configuration".into());
    }
    Ok(Some(path))
}
pub fn await_launch(ticket: &Path) -> Result<()> {
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    loop {
        if let Some(path) = bound_plan(ticket)? {
            let now = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_err(|_| "Invalid clock")?
                .as_millis() as u64;
            return hook_launch::launch(&path, now);
        }
        if std::time::Instant::now() >= deadline {
            return Err("Launch identity publication timed out; no provider started".into());
        }
        std::thread::sleep(Duration::from_millis(25));
    }
}
