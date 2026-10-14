//! Shared verified native slash-command submission for local CLI and Telegram.
//! A successful terminal write is submission, never proof a setting changed.
use super::{
    config::ControlConfig,
    hook_runtime::{self, Readiness, Registration},
    hooks::Harness,
};
use crate::{
    adapters::{
        harness_commands, harnesses, process,
        terminal_actions::{Action, ReceiptState},
        terminals::{self, Terminals},
    },
    core::layouts,
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
};
type Result<T> = std::result::Result<T, String>;
#[derive(Clone)]
pub struct Request {
    pub directory: PathBuf,
    pub registration: PathBuf,
    pub target: Value,
    pub command: String,
    pub arguments: String,
}
pub struct Plan {
    request: Request,
    registration: Registration,
    native: harness_commands::Plan,
    digest: String,
    sequence: i64,
}
impl Plan {
    pub fn fingerprint(&self) -> &str {
        &self.digest
    }
    pub fn review(&self) -> Value {
        json!({"operation":"native_harness_control","command":self.native.literal(),"target":self.registration.scope.terminal,"conversation":self.registration.scope.conversation,"fingerprint":self.digest,"may_invoke_model":self.native.may_invoke_model,"changes_context":self.native.changes_context,"may_require_interaction":self.native.may_require_interaction,"notice":self.native.submission_notice()})
    }
}
fn same_target(registration: &Registration, target: &Value, directory: &Path) -> Result<()> {
    if registration.directory != directory
        || registration.scope.terminal != terminals::identity(target)?
    {
        return Err("Registration does not match exact terminal and instance".into());
    }
    let session = &target["agent_session"];
    let observed = if session["kind"] == "id" {
        session["value"].as_str()
    } else {
        session.as_str()
    };
    if observed
        .filter(|s| !s.is_empty())
        .is_some_and(|s| s != registration.scope.conversation)
    {
        return Err("Target conversation changed".into());
    }
    Ok(())
}
fn executable_stamp(path: &Path) -> Result<Vec<i128>> {
    let m = std::fs::metadata(path).map_err(|_| "Harness executable unavailable")?;
    Ok(vec![
        m.dev() as i128,
        m.ino() as i128,
        m.len() as i128,
        m.mtime() as i128,
        m.mtime_nsec() as i128,
        m.ctime() as i128,
        m.ctime_nsec() as i128,
    ])
}
pub fn prepare(
    request: Request,
    config: &ControlConfig,
    terminals: &Terminals,
    environment: &BTreeMap<String, String>,
) -> Result<Plan> {
    config.validate()?;
    let current = ControlConfig::parse(
        &layouts::read_private(&request.directory.join("control.json"))?
            .ok_or("Instance configuration is missing")?,
    )?;
    if &current != config {
        return Err("Instance configuration changed; review again".into());
    }
    let registration = hook_runtime::inspect(&request.registration).map_err(|e| e.to_string())?;
    same_target(&registration, &request.target, &request.directory)?;
    if registration.scope.harness != Harness::Claude {
        return Err("This harness has no verified native command adapter".into());
    }
    let pid = registration
        .harness_process
        .as_ref()
        .ok_or("No verified harness process")?;
    let executable = terminals.proc_root.join(pid.pid.to_string()).join("exe");
    let before = executable_stamp(&executable)?;
    let actual =
        hook_runtime::verify_live_session(&registration, terminals).map_err(|e| e.to_string())?;
    same_target(&registration, &actual, &request.directory)?;
    let tool = harnesses::tool("claude").ok_or("Missing harness adapter")?;
    let launch = harnesses::launch_plan(
        &tool,
        &executable,
        Path::new(&config.agent_home),
        environment,
        &config.profiles,
    )?;
    let output = process::run(
        launch.command().arg("--version"),
        std::time::Duration::from_secs(5),
        4096,
    )
    .map_err(|_| "Cannot verify native command version")?;
    if !output.status.success() {
        return Err("Native version query failed".into());
    }
    let version = std::str::from_utf8(&output.stdout)
        .map_err(|_| "Invalid native version")?
        .trim();
    let native = harness_commands::plan("claude", version, &request.command, &request.arguments)
        .map_err(|e| e.to_string())?;
    if executable_stamp(&executable)? != before {
        return Err("Harness executable changed".into());
    }
    let ready = hook_runtime::ready_snapshot(&request.registration, &terminals.proc_root)
        .map_err(|e| e.to_string())?;
    if ready.scope != registration.scope || ready.state != Readiness::Idle {
        return Err("Harness readiness is not verified idle".into());
    }
    let digest=format!("{:x}",Sha256::digest(serde_json::to_vec(&json!({"registration":registration,"path":request.registration,"command":native.literal(),"executable":before,"config":config,"environment":launch.environment,"readiness":ready})).map_err(|_|"Cannot encode control review")?));
    Ok(Plan {
        request,
        registration,
        native,
        digest,
        sequence: ready.sequence,
    })
}
pub fn apply(
    plan: Plan,
    approved: &str,
    config: &ControlConfig,
    terminals: &Terminals,
    environment: &BTreeMap<String, String>,
) -> Result<String> {
    let current = prepare(plan.request.clone(), config, terminals, environment)?;
    if approved != plan.digest || current.digest != plan.digest {
        return Err("Native control review changed".into());
    }
    dispatch(plan, terminals)
}
fn dispatch(plan: Plan, terminals: &Terminals) -> Result<String> {
    let _active = hook_runtime::active_guard(&plan.request.directory).map_err(|e| e.to_string())?;
    let _installation = super::hook_install::lock(&plan.request.directory)?;
    let current = hook_runtime::inspect(&plan.request.registration).map_err(|e| e.to_string())?;
    if serde_json::to_value(&current).map_err(|_| "Invalid registration")?
        != serde_json::to_value(&plan.registration).map_err(|_| "Invalid registration")?
    {
        return Err("Hook registration changed; review again".into());
    }
    let ready = hook_runtime::ready_snapshot(&plan.request.registration, &terminals.proc_root)
        .map_err(|e| e.to_string())?;
    if ready.sequence != plan.sequence
        || ready.scope != plan.registration.scope
        || ready.state != Readiness::Idle
        || !hook_runtime::consume_ready_for_control(
            &plan.request.registration,
            &terminals.proc_root,
            ready.sequence,
        )
        .map_err(|e| e.to_string())?
    {
        return Err("Harness readiness is not verified idle".into());
    }
    let actual = hook_runtime::verify_live_session(&plan.registration, terminals)
        .map_err(|e| e.to_string())?;
    let receipt = terminals.action(&actual, Action::Send(plan.native.literal().into()))?;
    if !matches!(
        receipt.state,
        ReceiptState::Submitted | ReceiptState::Accepted
    ) {
        return Err("Native command submission is unconfirmed".into());
    }
    Ok(format!(
        "{} Native interaction may be required; readiness requires a new verified hook event.",
        plan.native.submission_notice()
    ))
}
pub fn submit(
    request: Request,
    config: &ControlConfig,
    terminals: &Terminals,
    environment: &BTreeMap<String, String>,
) -> Result<String> {
    let plan = prepare(request, config, terminals, environment)?;
    dispatch(plan, terminals)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn native_control_preview_pins_configuration_and_apply_consumes_idle_once() {
        use std::os::unix::fs::{PermissionsExt, symlink};
        struct Verified;
        impl hook_runtime::Verifier for Verified {
            fn verify(&mut self, _: &Registration) -> hook_runtime::Result<()> {
                Ok(())
            }
        }
        let dir = std::env::temp_dir().join(format!(
            "daedalus-control-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir(&dir).unwrap();
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).unwrap();
        let config = ControlConfig::fresh(&dir).unwrap();
        layouts::write_private(
            &dir.join("control.json"),
            &serde_json::to_vec(&config).unwrap(),
        )
        .unwrap();
        let proc = dir.join("proc");
        std::fs::create_dir_all(proc.join("42")).unwrap();
        let mut fields = vec!["0"; 20];
        fields[0] = "S";
        fields[2] = "42";
        fields[4] = "99";
        fields[5] = "42";
        fields[19] = "500";
        std::fs::write(
            proc.join("42/stat"),
            format!("42 (fixture) {}", fields.join(" ")),
        )
        .unwrap();
        let binary = dir.join("claude");
        std::fs::write(&binary, "#!/bin/sh\nprintf '%s\\n' '2.1.296'\n").unwrap();
        std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o700)).unwrap();
        symlink(&binary, proc.join("42/exe")).unwrap();
        let herdr = dir.join("herdr");
        std::fs::write(&herdr,format!("#!/bin/sh\ncase \"$*\" in *model*) printf sent >> '{}';; esac\nprintf '%s\\n' '{{\"result\":{{\"pane\":{{\"pane_id\":\"pane\",\"terminal_id\":\"generation\",\"pid\":42}}}}}}'\n",dir.join("sent").display())).unwrap();
        std::fs::set_permissions(&herdr, std::fs::Permissions::from_mode(0o700)).unwrap();
        let terminals = Terminals {
            herdr,
            proc_root: proc,
            ..Default::default()
        };
        let registration:Registration=serde_json::from_value(json!({"version":1,"id":"fixture","directory":dir,"scope":{"instance":config.agent_name,"terminal":{"backend":"herdr","endpoint":"example","pane":"pane","generation":"generation"},"harness":"claude","conversation":"session"},"harness_process":{"pid":42,"start_ticks":500},"events":["Stop"],"options":{"telegram_delivery":false,"telegram_progress":false,"notifications":false,"hindsight":false},"activity_lease_ms":30000,"pending_timeout_ms":30000,"hook_timeout_ms":12000})).unwrap();
        let path = hook_runtime::install(&registration).unwrap();
        hook_runtime::execute(
            br#"{"session_id":"session","hook_event_name":"Stop","last_assistant_message":"done"}"#
                .as_slice(),
            &registration,
            Some("done"),
            &mut Verified,
            &mut hook_runtime::SystemClock,
        )
        .unwrap();
        let request = Request {
            directory: dir.clone(),
            registration: path,
            target: json!({"backend":"herdr","session":"example","pane_id":"pane","terminal_id":"generation"}),
            command: "model".into(),
            arguments: "test-model".into(),
        };
        let env = BTreeMap::from([
            ("HOME".into(), dir.to_string_lossy().into_owned()),
            ("PATH".into(), "/usr/bin:/bin".into()),
            ("UNRELATED_SECRET".into(), "PRIVATE".into()),
        ]);
        let plan = prepare(request.clone(), &config, &terminals, &env).unwrap();
        assert!(!plan.review().to_string().contains("PRIVATE"));
        assert!(!dir.join("sent").exists());
        assert!(apply(plan, "wrong", &config, &terminals, &env).is_err());
        assert!(!dir.join("sent").exists());
        let plan = prepare(request.clone(), &config, &terminals, &env).unwrap();
        let sha = plan.fingerprint().to_owned();
        apply(plan, &sha, &config, &terminals, &env).unwrap();
        assert!(dir.join("sent").exists());
        let before = std::fs::read(dir.join("sent")).unwrap();
        assert!(prepare(request.clone(), &config, &terminals, &env).is_err());
        // A later idle event does not revive approval of the old observation.
        hook_runtime::execute(br#"{"session_id":"session","hook_event_name":"Stop","last_assistant_message":"later"}"#.as_slice(),&registration,Some("later"),&mut Verified,&mut hook_runtime::SystemClock).unwrap();
        let fresh = prepare(request.clone(), &config, &terminals, &env).unwrap();
        assert!(apply(fresh, &sha, &config, &terminals, &env).is_err());
        assert_eq!(std::fs::read(dir.join("sent")).unwrap(), before);
        let mut changed = config.clone();
        changed.agent_name = "changed".into();
        layouts::write_private(
            &dir.join("control.json"),
            &serde_json::to_vec(&changed).unwrap(),
        )
        .unwrap();
        assert!(prepare(request, &config, &terminals, &env).is_err());
        std::fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn target_scope_checks_object_and_legacy_session_ids() {
        let mut r:Registration=serde_json::from_value(json!({"version":1,"id":"fixture","directory":"/tmp/fixture","scope":{"instance":"example","terminal":{"backend":"herdr","endpoint":"example","pane":"pane","generation":"generation"},"harness":"claude","conversation":"session"},"events":["Stop"],"options":{"telegram_delivery":false,"telegram_progress":false,"notifications":false,"hindsight":false},"activity_lease_ms":30000,"pending_timeout_ms":30000,"hook_timeout_ms":12000})).unwrap();
        let mut pane = json!({"backend":"herdr","session":"example","pane_id":"pane","terminal_id":"generation","agent_session":{"kind":"id","value":"session"}});
        assert!(same_target(&r, &pane, Path::new("/tmp/fixture")).is_ok());
        pane["agent_session"]["value"] = "other".into();
        assert!(same_target(&r, &pane, Path::new("/tmp/fixture")).is_err());
        pane["agent_session"] = "session".into();
        assert!(same_target(&r, &pane, Path::new("/tmp/fixture")).is_ok());
        r.scope.terminal.generation = "recycled".into();
        assert!(same_target(&r, &pane, Path::new("/tmp/fixture")).is_err());
    }
}
