//! Reviewed native hook installation for the existing dialog protocol.
use super::{
    backend::Source,
    config::ControlConfig,
    hook_install::{Preview, Receipt},
    hook_runtime::Options,
    session_menu::{Outcome, View},
    tree,
};
use serde_json::{Value, json};
use std::collections::VecDeque;
use std::path::PathBuf;
type Result<T> = std::result::Result<T, String>;
#[derive(Clone)]
pub struct Spec {
    pub pane: Value,
    pub profile: PathBuf,
    pub transcript: PathBuf,
    pub options: Options,
    pub goal_id: Option<String>,
}
struct MemorySpec {
    receipt: Receipt,
    registration: super::hook_runtime::Registration,
    config: super::memory::Config,
    expected: Option<super::memory::Config>,
}
enum Stage {
    HerdrReview(Box<super::herdr_hooks::Plan>),
    HerdrApply(Box<super::herdr_hooks::Plan>),
    RemovalChoose(Vec<super::hook_uninstall::Entry>),
    RemovalReview(Box<super::hook_uninstall::Plan>),
    RemovalApply(Box<super::hook_uninstall::Plan>),
    RemovalResult(PathBuf),
    Pane(Vec<Value>),
    BatchStart(Vec<Value>),
    Profile(Value),
    Transcript(Spec),
    Preset(Spec),
    Options(Spec),
    Goal(Spec),
    Review(Box<Preview>),
    Apply(Box<Preview>),
    Result(Receipt),
    Recipients {
        receipt: Receipt,
        choices: Vec<Value>,
    },
    BindingReview(Box<super::telegram_admin::Plan>, Receipt),
    BindingApply(Box<super::telegram_admin::Plan>, Receipt),
    MemoryEndpoint(Box<MemorySpec>),
    MemoryCredential(Box<MemorySpec>),
    MemoryBank(Box<MemorySpec>),
    MemoryReview(Box<super::memory::Plan>, Receipt),
    MemoryApply(Box<super::memory::Plan>, Receipt),
}
struct Pending {
    id: u64,
    mode: &'static str,
    choices: Vec<String>,
    stage: Stage,
}
#[derive(Default)]
pub struct Menu {
    pending: Option<Pending>,
    batch: VecDeque<Value>,
}
impl Menu {
    pub fn pending(&self) -> bool {
        self.pending.is_some()
    }
    pub fn cancel(&mut self) {
        self.pending = None;
        self.batch.clear();
    }
    fn frame(
        &mut self,
        seq: &mut u64,
        mode: &'static str,
        title: &str,
        extra: Value,
        stage: Stage,
    ) -> Result<Outcome> {
        *seq = seq.checked_add(1).ok_or("Dialog sequence exhausted")?;
        let mut frame = json!({"version":2,"kind":"dialog","id":*seq,"mode":mode,"title":title});
        frame
            .as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        let choices = frame["options"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|v| v[0].as_str().map(str::to_owned))
            .collect();
        self.pending = Some(Pending {
            id: *seq,
            mode,
            choices,
            stage,
        });
        Ok(Outcome {
            frames: vec![frame],
            ..Default::default()
        })
    }
    pub fn start(&mut self, view: View, seq: &mut u64) -> Result<Outcome> {
        if self.pending() {
            return Err("Finish the current hook dialog first".into());
        }
        self.batch.clear();
        if let Some(pane) = view.selected {
            return self.profile(pane, seq);
        }
        let panes = view.snapshot["panes"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        if panes.is_empty() {
            return Err("No live terminal is available for hook registration".into());
        }
        let options = panes
            .iter()
            .enumerate()
            .map(|(i, p)| {
                json!([
                    i.to_string(),
                    tree::title(p, ""),
                    "Native conversation and foreground process will be verified"
                ])
            })
            .collect::<Vec<_>>();
        self.frame(
            seq,
            "choose",
            "Configure hooks for which live terminal?",
            json!({"options":options}),
            Stage::Pane(panes),
        )
    }
    pub fn start_all(&mut self, view: View, seq: &mut u64) -> Result<Outcome> {
        if self.pending() {
            return Err("Finish the current hook dialog first".into());
        }
        self.batch.clear();
        let mut seen = std::collections::HashSet::new();
        let panes = view.snapshot["panes"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|pane| {
                matches!(
                    pane["agent"]
                        .as_str()
                        .unwrap_or("")
                        .to_ascii_lowercase()
                        .as_str(),
                    "claude" | "codex" | "grok" | "cursor" | "hermes" | "pi" | "opencode"
                )
            })
            .filter(|pane| {
                seen.insert((
                    pane["backend"].as_str().unwrap_or("").to_owned(),
                    pane["server"].as_str().unwrap_or("").to_owned(),
                    pane["pane_id"].as_str().unwrap_or("").to_owned(),
                    pane["terminal_id"].as_str().unwrap_or("").to_owned(),
                ))
            })
            .cloned()
            .collect::<Vec<_>>();
        if panes.is_empty() {
            return Err("No supported live harness sessions were detected. Start an agent and refresh discovery.".into());
        }
        let labels = panes
            .iter()
            .enumerate()
            .map(|(index, pane)| format!("{}. {}", index + 1, tree::title(pane, "")))
            .collect::<Vec<_>>()
            .join("\n");
        self.frame(seq,"choose","Install hooks for detected sessions",
            json!({"description":format!("Each session requires its own verified profile, transcript and installation review. Unsupported platform/provider combinations are rejected without modifying files.\n\n{labels}"),
                "options":[["start","Review sessions one by one","No hook is installed automatically"],["cancel","Cancel","Leave every profile unchanged"]]}),
            Stage::BatchStart(panes))
    }
    fn next_batch(&mut self, seq: &mut u64) -> Result<Outcome> {
        match self.batch.pop_front() {
            Some(pane) => self.profile(pane, seq),
            None => Ok(Outcome {
                message: "All selected hook sessions were reviewed".into(),
                refresh: true,
                ..Default::default()
            }),
        }
    }
    fn profile(&mut self, pane: Value, seq: &mut u64) -> Result<Outcome> {
        self.frame(
            seq,
            "input",
            "Native hook profile file (absolute path)",
            json!({"secret":false}),
            Stage::Profile(pane),
        )
    }
    fn options(&mut self, spec: Spec, seq: &mut u64) -> Result<Outcome> {
        let toggles = [
            (
                "delivery",
                "Telegram final responses",
                spec.options.telegram_delivery,
            ),
            (
                "progress",
                "Telegram progress",
                spec.options.telegram_progress,
            ),
            (
                "notifications",
                "Attention and permission notifications",
                spec.options.notifications,
            ),
            (
                "hindsight",
                "Optional Hindsight memory",
                spec.options.hindsight,
            ),
        ];
        let mut options = toggles
            .iter()
            .map(|(id, label, on)| {
                json!([
                    id,
                    format!("{} {label}", if *on { "[x]" } else { "[ ]" }),
                    "Toggle this hook feature"
                ])
            })
            .collect::<Vec<_>>();
        options.push(json!([
            "goal",
            "Goal binding",
            spec.goal_id
                .as_deref()
                .unwrap_or("None; native adapter support required")
        ]));
        options.push(json!([
            "review",
            "Review hook changes",
            "No profile changes before confirmation"
        ]));
        self.frame(
            seq,
            "choose",
            "Hook features",
            json!({
                "options": options,
                "description": "Delivery and progress are preselected. Telegram requires a separate authorized recipient binding. Hooks target one verified conversation; memory and notifications are optional."
            }),
            Stage::Options(spec),
        )
    }
    fn installed(&mut self, receipt: Receipt, seq: &mut u64, message: &str) -> Result<Outcome> {
        let mut options = vec![
            json!(["done", "Done", "Keep the installed profile"]),
            json!([
                "telegram",
                "Connect Telegram",
                "Select an explicit authorized recipient"
            ]),
            json!([
                "memory",
                "Configure Hindsight",
                "Set the endpoint and registration bank"
            ]),
        ];
        if !self.batch.is_empty() {
            options.push(json!([
                "next",
                "Next detected agent",
                "Review the next native profile"
            ]));
        }
        let mut outcome = self.frame(
            seq,
            "choose",
            "Hook profile installed",
            json!({
                "options": options,
                "description": message
            }),
            Stage::Result(receipt),
        )?;
        outcome.message = message.into();
        outcome.refresh = true;
        Ok(outcome)
    }

    fn start_memory<S: Source>(
        &mut self,
        source: &mut S,
        receipt: Receipt,
        seq: &mut u64,
    ) -> Result<Outcome> {
        let (registration, current) = source.memory_settings(&receipt.registration)?;
        let existing = current.is_some();
        let expected = current.clone();
        let config = current.unwrap_or(super::memory::Config {
            version: 1,
            enabled: true,
            endpoint: String::new(),
            credential_file: None,
            bindings: vec![],
            recall_query: "Relevant context for this agent conversation".into(),
            timeout_ms: 5000,
        });
        let spec = Box::new(MemorySpec {
            receipt,
            registration,
            config,
            expected,
        });
        if existing {
            self.frame(
                seq,
                "input",
                "Hindsight bank for this registration",
                json!({
                    "secret": false,
                    "description": "Existing endpoint, credential file and other memory bindings are retained."
                }),
                Stage::MemoryBank(spec),
            )
        } else {
            self.frame(
                seq,
                "input",
                "Hindsight API endpoint",
                json!({"secret": false}),
                Stage::MemoryEndpoint(spec),
            )
        }
    }

    fn review_memory<S: Source>(
        &mut self,
        source: &mut S,
        mut spec: MemorySpec,
        bank: &str,
        seq: &mut u64,
    ) -> Result<Outcome> {
        let (registration, current) = source.memory_settings(&spec.receipt.registration)?;
        if serde_json::to_value(&registration).map_err(|_| "Invalid registration")?
            != serde_json::to_value(&spec.registration).map_err(|_| "Invalid registration")?
        {
            return Err("Hook registration changed; reopen memory setup".into());
        }
        // Preserve other registrations and endpoint settings; refuse a concurrent edit.
        if serde_json::to_value(&current).ok() != serde_json::to_value(&spec.expected).ok() {
            return Err("Memory settings changed; reopen setup".into());
        }
        spec.config.enabled = true;
        spec.config
            .bindings
            .retain(|binding| binding.registration_id != registration.id);
        spec.config.bindings.push(super::memory::Binding {
            registration_id: registration.id,
            scope: registration.scope,
            bank: bank.into(),
        });
        let plan = source.prepare_memory(spec.expected, spec.config)?;
        let text = super::dialog_text::format(&plan.review())
            .map_err(|_| "Cannot format memory review")?;
        self.frame(
            seq,
            "output",
            "Review Hindsight configuration",
            json!({"text": text}),
            Stage::MemoryReview(Box::new(plan), spec.receipt),
        )
    }

    fn start_recipients<S: Source>(
        &mut self,
        source: &mut S,
        receipt: Receipt,
        seq: &mut u64,
    ) -> Result<Outcome> {
        let super::telegram_admin::Reply::Data(data) =
            source.telegram(super::telegram_admin::Request::Bindings {
                registration: receipt.registration.clone(),
            })?
        else {
            return Err("Unexpected recipient response".into());
        };
        let choices = data["recipients"]
            .as_array()
            .cloned()
            .ok_or("Invalid recipient list")?;
        if choices.is_empty() {
            return Ok(Outcome {
                message: "Hooks installed. Configure an explicit Read+Send grant and route its chat to this exact conversation before connecting delivery.".into(),
                refresh: true,
                ..Default::default()
            });
        }
        let options = choices
            .iter()
            .enumerate()
            .map(|(index, choice)| {
                json!([
                    index.to_string(),
                    format!(
                        "Chat {} · actor {}",
                        choice["chat"].as_str().unwrap_or(""),
                        choice["actor"].as_str().unwrap_or("")
                    ),
                    format!("Route revision {}", choice["route_revision"])
                ])
            })
            .collect::<Vec<_>>();
        self.frame(
            seq,
            "choose",
            "Choose hook recipient",
            json!({"options": options}),
            Stage::Recipients { receipt, choices },
        )
    }

    fn review_recipient<S: Source>(
        &mut self,
        source: &mut S,
        receipt: Receipt,
        choices: &[Value],
        selected: &str,
        seq: &mut u64,
    ) -> Result<Outcome> {
        let index = selected.parse::<usize>().map_err(|_| "Invalid recipient")?;
        let choice = choices.get(index).ok_or("Recipient selection changed")?;
        let chat = choice["chat"]
            .as_str()
            .ok_or("Missing chat")?
            .parse()
            .map_err(|_| "Invalid chat")?;
        let actor = choice["actor"]
            .as_str()
            .ok_or("Missing actor")?
            .parse()
            .map_err(|_| "Invalid actor")?;
        let super::telegram_admin::Reply::Plan(plan) =
            source.telegram(super::telegram_admin::Request::Bind {
                registration: receipt.registration.clone(),
                chat,
                actor,
            })?
        else {
            return Err("Unexpected binding review".into());
        };
        if !matches!(*plan, super::telegram_admin::Plan::Binding(_)) {
            return Err("Unexpected binding plan".into());
        }
        let text =
            super::dialog_text::format(&plan.review()).map_err(|_| "Cannot format recipient")?;
        self.frame(
            seq,
            "output",
            "Review hook recipient",
            json!({"text": text}),
            Stage::BindingReview(plan, receipt),
        )
    }

    fn review_hooks<S: Source>(
        &mut self,
        source: &mut S,
        config: &ControlConfig,
        spec: Spec,
        seq: &mut u64,
    ) -> Result<Outcome> {
        let plan = source.prepare_hooks(config, spec)?;
        let summary = json!({
            "profile": plan.profile,
            "registration": plan.registration.id,
            "harness": plan.registration.scope.harness,
            "options": plan.registration.options,
            "goal_id": plan.registration.goal_id,
            "before_sha": plan.before_sha,
            "after_sha": plan.after_sha,
            "note": "Existing unrelated handlers are retained. Apply rechecks process/terminal identity and the exact reviewed profile bytes."
        });
        let text = super::dialog_text::format(&summary).map_err(|_| "Cannot format hook review")?;
        self.frame(
            seq,
            "output",
            "Review hook profile",
            json!({"text": text}),
            Stage::Review(Box::new(plan)),
        )
    }

    pub fn herdr<S: Source>(
        &mut self,
        config: &ControlConfig,
        harness: &str,
        source: &mut S,
        seq: &mut u64,
    ) -> Result<Outcome> {
        if self.pending() {
            return Err("Finish the current hook dialog first".into());
        }
        let plan = source.prepare_herdr_hooks(config, harness)?;
        let text =
            super::dialog_text::format(&plan.review()).map_err(|_| "Cannot format Herdr review")?;
        self.frame(
            seq,
            "output",
            "Review Herdr integration",
            json!({"text":text}),
            Stage::HerdrReview(Box::new(plan)),
        )
    }

    pub fn remove<S: Source>(
        &mut self,
        harness: &str,
        source: &mut S,
        seq: &mut u64,
    ) -> Result<Outcome> {
        if self.pending() {
            return Err("Finish the current hook dialog first".into());
        }
        let entries = source.managed_hooks(harness)?;
        if entries.is_empty() {
            return Ok(Outcome {
                message: concat!(
                    "No unchanged managed installation is available for removal. ",
                    "Edited profiles are preserved; retain their backups for manual review."
                )
                .into(),
                ..Default::default()
            });
        }
        let options = entries
            .iter()
            .enumerate()
            .map(|(index, entry)| {
                json!([
                    index.to_string(),
                    entry.registration,
                    entry.profile.display().to_string()
                ])
            })
            .collect::<Vec<_>>();
        self.frame(
            seq,
            "choose",
            "Remove which managed installation?",
            json!({"options":options}),
            Stage::RemovalChoose(entries),
        )
    }

    pub fn reply<S: Source>(
        &mut self,
        request: &Value,
        source: &mut S,
        config: &ControlConfig,
        seq: &mut u64,
    ) -> Result<Outcome> {
        let pending = self.pending.as_ref().ok_or("No hook dialog is pending")?;
        if request["op"] != "reply" || request["id"].as_u64() != Some(pending.id) {
            return Err("Unexpected hook reply".into());
        }
        let value = request.get("value").ok_or("Missing hook reply")?;
        if !value.is_null() {
            let text = value.as_str().ok_or("Invalid hook reply")?;
            let valid = match pending.mode {
                "choose" => pending.choices.iter().any(|choice| choice == text),
                "input" => {
                    !text.is_empty() && text.len() <= 4096 && !text.chars().any(char::is_control)
                }
                "output" => text == "close",
                _ => false,
            };
            if !valid {
                return Err("Invalid hook reply".into());
            }
        }
        let pending = self.pending.take().unwrap();
        if value.is_null() {
            return Ok(Outcome::default());
        }
        let text = value.as_str().unwrap();
        match pending.stage {
            Stage::BatchStart(panes) => {
                if text == "cancel" { return Ok(Outcome::default()); }
                self.batch = panes.into();
                self.next_batch(seq)
            }
            Stage::HerdrReview(plan) => self.frame(seq, "choose", "Confirm Herdr integration installation",
                json!({"options":[["cancel","Cancel","Keep the profile unchanged"],["yes","Install or update","Back up reviewed files, run Herdr and verify its status"]]}), Stage::HerdrApply(plan)),
            Stage::HerdrApply(plan) => {
                if text == "cancel" { return Ok(Outcome::default()); }
                let approval = plan.fingerprint()?;
                let receipt = source.apply_herdr_hooks(*plan, &approval)?;
                Ok(Outcome { message: format!("{} · Backup: {}", receipt.diagnostic, receipt.backup.display()), refresh: true, ..Default::default() })
            }
            Stage::RemovalChoose(entries)=>{
                let index=text.parse::<usize>().map_err(|_|"Invalid installation selection")?;
                let entry=entries.get(index).ok_or("Installation selection changed")?;
                let plan=source.prepare_hook_removal(&entry.receipt)?;
                let review=super::dialog_text::format(&plan.review()).map_err(|_|"Cannot format removal review")?;
                self.frame(seq,"output","Review managed hook removal",json!({"text":review}),Stage::RemovalReview(Box::new(plan)))
            }
            Stage::RemovalReview(plan)=>self.frame(
                seq,"choose","Confirm managed hook removal",
                json!({"options":[
                    ["cancel","Cancel","Keep the profile and registration unchanged"],
                    ["yes","Remove","Restore the reviewed backup and disable registration features"]
                ]}),Stage::RemovalApply(plan)
            ),
            Stage::RemovalApply(plan)=>{
                if text=="cancel"{return Ok(Outcome::default());}
                let approval=plan.approval();
                let receipt=source.apply_hook_removal(*plan,&approval)?;
                self.frame(seq,"choose","Managed hooks removed",
                    json!({"description":"Restart or reload the harness to discard cached handlers. Queued deliveries remain recorded.","options":[
                        ["done","Done","Keep the reviewed removal"],
                        ["rollback","Restore installation","Restore only if both resulting files are unchanged"]
                    ]}),Stage::RemovalResult(receipt))
            }
            Stage::RemovalResult(path)=>{
                let message=if text=="rollback"{source.rollback_hook_removal(&path)?;"Managed hook installation restored"}else{"Managed hooks removed"};
                Ok(Outcome{message:message.into(),refresh:true,..Default::default()})
            }

            Stage::Pane(panes) => {
                let index = text.parse::<usize>().map_err(|_| "Invalid pane selection")?;
                let pane = panes.get(index).ok_or("Pane selection changed")?.clone();
                self.profile(pane, seq)
            }
            Stage::Profile(pane) => {
                let profile = PathBuf::from(text);
                crate::core::layouts::check_path(&profile)?;
                self.frame(
                    seq,
                    "input",
                    "Native transcript file (absolute path)",
                    json!({"secret": false}),
                    Stage::Transcript(Spec {
                        pane,
                        profile,
                        transcript: PathBuf::new(),
                        options: Options::guided(),
                        goal_id: None,
                    }),
                )
            }
            Stage::Transcript(mut spec) => {
                spec.transcript = text.into();
                crate::core::layouts::check_path(&spec.transcript)?;
                self.frame(seq, "choose", "Choose hook setup", json!({
                    "description": "The standard set tracks lifecycle and enables Telegram final replies and progress. Delivery still needs an authorized chat binding.",
                    "options": [
                        ["defaults", "Install standard hooks", "Review the recommended settings"],
                        ["custom", "Choose features", "Adjust Telegram, notifications, memory and goal binding"]
                    ]
                }), Stage::Preset(spec))
            }
            Stage::Preset(spec) => {
                if text == "custom" {
                    self.options(spec, seq)
                } else {
                    self.review_hooks(source, config, spec, seq)
                }
            }
            Stage::Options(mut spec) => {
                match text {
                    "delivery" => spec.options.telegram_delivery = !spec.options.telegram_delivery,
                    "progress" => spec.options.telegram_progress = !spec.options.telegram_progress,
                    "notifications" => spec.options.notifications = !spec.options.notifications,
                    "hindsight" => spec.options.hindsight = !spec.options.hindsight,
                    "goal" => return self.frame(
                        seq,
                        "input",
                        "Existing goal ID, or none to clear",
                        json!({"secret": false}),
                        Stage::Goal(spec),
                    ),
                    _ => return self.review_hooks(source, config, spec, seq),
                }
                self.options(spec, seq)
            }
            Stage::Goal(mut spec) => {
                spec.goal_id = if text == "none" { None } else { Some(text.into()) };
                self.options(spec, seq)
            }
            Stage::Review(plan) => self.frame(
                seq,
                "choose",
                "Confirm action",
                json!({
                    "description": "Install the reviewed native hook profile? Native tools may require restart or their own trust review.",
                    "options": [
                        ["cancel", "Cancel", "Leave native hook files unchanged"],
                        ["yes", "Continue", "Install exactly the reviewed changes"]
                    ]
                }),
                Stage::Apply(plan),
            ),
            Stage::Apply(plan) => {
                if text == "cancel" {
                    self.batch.clear();
                    return Ok(Outcome::default());
                }
                let receipt = source.apply_hooks(*plan)?;
                let mut options = vec![json!(["done", "Done", "Keep the installed profile"]),
                    json!(["telegram", "Connect Telegram", "Choose an explicitly authorized recipient and review its route"]),
                    json!(["memory", "Configure Hindsight", "Set an explicit endpoint and memory bank for this registration"]),
                    json!(["rollback", "Restore previous profile", "Restore only if installed bytes still match; preserve later edits"])];
                if !self.batch.is_empty() { options.push(json!(["next","Next detected agent","Review the next native profile"])); }
                self.frame(
                    seq,
                    "choose",
                    "Hook profile installed",
                    json!({
                        "options": options,
                        "description": format!("Profile: {}. Native tool trust/restart requirements still apply.", receipt.profile.display())
                    }),
                    Stage::Result(receipt),
                )
            }
            Stage::Result(receipt) => match text {
                "memory" => self.start_memory(source, receipt, seq),
                "telegram" => self.start_recipients(source, receipt, seq),
                "next" => self.next_batch(seq),
                _ => {
                    self.batch.clear();
                    let message = if text == "rollback" {
                        source.rollback_hooks(receipt)?;
                        "Previous hook profile restored"
                    } else {
                        "Hook profile installed"
                    };
                    Ok(Outcome { message: message.into(), refresh: true, ..Default::default() })
                }
            },
            Stage::Recipients { receipt, choices } => {
                self.review_recipient(source, receipt, &choices, text, seq)
            }
            Stage::MemoryEndpoint(mut spec) => {
                spec.config.endpoint = text.into();
                self.frame(
                    seq,
                    "input",
                    "Private credential file (absolute path), or none",
                    json!({
                        "secret": false,
                        "description": "Enter a private file path, never paste the token here."
                    }),
                    Stage::MemoryCredential(spec),
                )
            }
            Stage::MemoryCredential(mut spec) => {
                spec.config.credential_file = if text == "none" {
                    None
                } else {
                    let path = PathBuf::from(text);
                    crate::core::layouts::check_path(&path)?;
                    Some(path)
                };
                self.frame(
                    seq,
                    "input",
                    "Hindsight bank for this registration",
                    json!({"secret": false}),
                    Stage::MemoryBank(spec),
                )
            }
            Stage::MemoryBank(spec) => self.review_memory(source, *spec, text, seq),
            Stage::MemoryReview(plan, receipt) => self.frame(
                seq,
                "choose",
                "Confirm Hindsight configuration",
                json!({
                    "options": [
                        ["cancel", "Cancel", "Keep memory configuration unchanged"],
                        ["yes", "Save", "Enable the reviewed endpoint and exact registration bank"]
                    ]
                }),
                Stage::MemoryApply(plan, receipt),
            ),
            Stage::MemoryApply(plan, receipt) => {
                if text == "cancel" {
                    return Ok(Outcome::default());
                }
                source.apply_memory(*plan)?;
                self.installed(receipt, seq, "Hindsight configuration saved. Network connectivity has not been tested.")
            }
            Stage::BindingReview(plan, receipt) => self.frame(
                seq,
                "choose",
                "Confirm hook recipient",
                json!({
                    "options": [
                        ["cancel", "Cancel", "Keep delivery recipients unchanged"],
                        ["yes", "Connect", "Save the exact reviewed recipient and route"]
                    ]
                }),
                Stage::BindingApply(plan, receipt),
            ),
            Stage::BindingApply(plan, receipt) => {
                if text == "cancel" {
                    return Ok(Outcome::default());
                }
                let approval = plan.approval();
                source.telegram(super::telegram_admin::Request::Apply { plan, approval })?;
                self.installed(receipt, seq, "Hook recipient saved. A running Telegram service reloads the binding; check its status and the selected chat route before sending a message.")
            }
        }
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::super::{
        backend::Observations, hook_install, hook_runtime::ProcessIdentity, hooks::Harness,
    };
    use super::*;
    use crate::{
        adapters::terminals::Terminals,
        core::identity::{Backend, TerminalId},
    };
    use std::{
        fs,
        os::unix::fs::PermissionsExt,
        sync::atomic::{AtomicU64, Ordering},
    };
    static NEXT: AtomicU64 = AtomicU64::new(0);
    struct Mock {
        dir: PathBuf,
        terminals: Terminals,
        previews: usize,
        applies: usize,
        fail: bool,
    }
    impl Mock {
        fn new() -> Self {
            let dir = std::env::temp_dir().join(format!(
                "daedalus-hook-menu-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir(&dir).unwrap();
            fs::set_permissions(&dir, fs::Permissions::from_mode(0o700)).unwrap();
            fs::create_dir_all(dir.join("proc/42")).unwrap();
            let mut fields = vec!["0"; 20];
            fields[0] = "S";
            fields[2] = "42";
            fields[4] = "99";
            fields[5] = "42";
            fields[19] = "500";
            fs::write(
                dir.join("proc/42/stat"),
                format!("42 (harness) {}", fields.join(" ")),
            )
            .unwrap();
            let binary = dir.join("herdr");
            fs::write(&binary,"#!/bin/sh\nprintf '%s\\n' '{\"result\":{\"pane\":{\"pane_id\":\"pane\",\"terminal_id\":\"terminal\",\"pid\":42}}}'\n").unwrap();
            fs::set_permissions(&binary, fs::Permissions::from_mode(0o700)).unwrap();
            Self {
                terminals: Terminals {
                    herdr: binary,
                    proc_root: dir.join("proc"),
                    ..Default::default()
                },
                dir,
                previews: 0,
                applies: 0,
                fail: false,
            }
        }
    }
    impl Drop for Mock {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.dir);
        }
    }
    impl Source for Mock {
        fn prepare_herdr_hooks(
            &mut self,
            _: &ControlConfig,
            harness: &str,
        ) -> Result<super::super::herdr_hooks::Plan> {
            self.previews += 1;
            serde_json::from_value(json!({
                "request":{"binary":self.dir.join("herdr"),"home":self.dir,"profile":self.dir.join("profile"),"directory":self.dir,"harness":harness},
                "status":{"state":"outdated","note":"fixture","path":self.dir.join("profile/hook")},
                "paths":[self.dir.join("profile/hook")],"binary_sha":"fixture","before":[]
            })).map_err(|_| "Invalid test plan".into())
        }
        fn apply_herdr_hooks(
            &mut self,
            plan: super::super::herdr_hooks::Plan,
            approval: &str,
        ) -> Result<super::super::herdr_hooks::Receipt> {
            assert_eq!(plan.fingerprint().unwrap(), approval);
            self.applies += 1;
            Ok(super::super::herdr_hooks::Receipt {
                current: !self.fail,
                after: plan.status,
                backup: self.dir.join("backup"),
                diagnostic: if self.fail {
                    "Installation not verified"
                } else {
                    "Verified by Herdr"
                }
                .into(),
            })
        }

        fn memory_settings(
            &mut self,
            _: &std::path::Path,
        ) -> Result<(
            super::super::hook_runtime::Registration,
            Option<super::super::memory::Config>,
        )> {
            let registration = super::super::hook_runtime::Registration {
                version: 1,
                id: "fixture-hook".into(),
                directory: self.dir.clone(),
                scope: crate::core::goal_store::Scope {
                    instance: "fixture".into(),
                    terminal: TerminalId {
                        backend: Backend::Herdr,
                        endpoint: "example".into(),
                        pane: "pane".into(),
                        generation: "terminal".into(),
                    },
                    harness: Harness::Claude,
                    conversation: "session".into(),
                },
                goal_id: None,
                harness_process: None,
                server_emitter: None,
                transcript: None,
                events: vec![super::super::hooks::EventKind::Stop],
                options: Options {
                    hindsight: true,
                    ..Default::default()
                },
                activity_lease_ms: 1000,
                pending_timeout_ms: 1000,
                hook_timeout_ms: 1000,
            };
            Ok((registration, super::super::memory::load(&self.dir)?))
        }
        fn prepare_memory(
            &mut self,
            expected: Option<super::super::memory::Config>,
            config: super::super::memory::Config,
        ) -> Result<super::super::memory::Plan> {
            assert_eq!(
                serde_json::to_value(expected).unwrap(),
                serde_json::to_value(super::super::memory::load(&self.dir)?).unwrap()
            );
            super::super::memory::Plan::prepare(&self.dir, config)
        }
        fn apply_memory(&mut self, plan: super::super::memory::Plan) -> Result<Value> {
            let approval = plan.approval();
            plan.apply(&approval)
        }
        fn telegram(
            &mut self,
            request: super::super::telegram_admin::Request,
        ) -> Result<super::super::telegram_admin::Reply> {
            match request {
                super::super::telegram_admin::Request::Bindings { registration } => {
                    assert_eq!(registration, self.dir.join("registration"));
                    Ok(super::super::telegram_admin::Reply::Data(
                        json!({"recipients":[]}),
                    ))
                }
                _ => panic!("A recipient must never be inferred or applied automatically"),
            }
        }
        fn refresh(&mut self, _: &ControlConfig) -> Result<Observations> {
            Ok(Observations::default())
        }
        fn preview(&mut self, _: &Value, _: usize) -> Result<String> {
            unreachable!()
        }
        fn save_layouts(&mut self, _: &crate::core::layouts::Layouts) -> Result<()> {
            unreachable!()
        }
        fn prepare_hooks(&mut self, _: &ControlConfig, spec: Spec) -> Result<Preview> {
            self.previews += 1;
            hook_install::preview(
                &hook_install::Request {
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
                    directory: self.dir.clone(),
                    profile: spec.profile,
                    binary: self.terminals.herdr.clone(),
                    transcript: Some(spec.transcript),
                    server_emitter: None,
                    registration_id: "hook-fixture".into(),
                    options: spec.options,
                    goal_id: spec.goal_id,
                    harness_version: "2.1.295".into(),
                },
                &self.terminals,
            )
        }
        fn apply_hooks(&mut self, plan: Preview) -> Result<Receipt> {
            self.applies += 1;
            if self.fail {
                return Err("Process identity changed".into());
            }
            assert!(plan.registration.options.telegram_delivery);
            assert!(plan.registration.options.telegram_progress);
            assert!(!plan.registration.options.notifications);
            assert!(!plan.registration.options.hindsight);
            Ok(Receipt {
                profile: plan.profile,
                backup: self.dir.join("backup"),
                after_sha: plan.after_sha,
                registration: self.dir.join("registration"),
            })
        }
    }
    fn view() -> View {
        View {
            snapshot: json!({"panes":[]}),
            health: json!({}),
            layouts: Default::default(),
            selected: Some(
                json!({"backend":"herdr","session":"example","pane_id":"pane","terminal_id":"terminal"}),
            ),
            checkpoint: None,
            node: String::new(),
        }
    }
    #[test]
    fn fleet_setup_lists_only_agent_sessions_and_does_not_install_before_review() {
        let (mut menu, mut mock, mut seq) = (Menu::default(), Mock::new(), 0);
        let mut fleet = view();
        fleet.selected = None;
        fleet.snapshot = json!({"panes":[
            {"backend":"herdr","pane_id":"shell","terminal_id":"shell","agent":"bash"},
            {"backend":"herdr","pane_id":"pane","terminal_id":"terminal","agent":"claude"}
        ]});
        let start = menu.start_all(fleet, &mut seq).unwrap();
        assert_eq!(
            start.frames[0]["title"],
            "Install hooks for detected sessions"
        );
        assert!(
            !start.frames[0]["description"]
                .as_str()
                .unwrap()
                .contains("shell")
        );
        assert_eq!(mock.applies, 0);
        let profile = reply(&mut menu, &mut mock, &mut seq, json!("start")).unwrap();
        assert_eq!(
            profile.frames[0]["title"],
            "Native hook profile file (absolute path)"
        );
        menu.cancel();
        assert!(!menu.pending());
        assert!(menu.batch.is_empty());
    }
    #[test]
    fn fleet_setup_requires_separate_review_for_each_session() {
        let (mut menu, mut mock, mut seq) = (Menu::default(), Mock::new(), 0);
        let mut fleet = view();
        fleet.selected = None;
        let pane =
            json!({"backend":"herdr","pane_id":"pane","terminal_id":"terminal","agent":"claude"});
        let mut other = pane.clone();
        other["pane_id"] = json!("other");
        fleet.snapshot = json!({"panes":[pane,other]});
        menu.start_all(fleet, &mut seq).unwrap();
        reply(&mut menu, &mut mock, &mut seq, json!("start")).unwrap();
        let profile = mock.dir.join("hooks.json");
        let transcript = mock.dir.join("transcript.jsonl");
        reply(&mut menu, &mut mock, &mut seq, json!(profile)).unwrap();
        reply(&mut menu, &mut mock, &mut seq, json!(transcript)).unwrap();
        reply(&mut menu, &mut mock, &mut seq, json!("defaults")).unwrap();
        assert_eq!(mock.applies, 0);
        reply(&mut menu, &mut mock, &mut seq, json!("close")).unwrap();
        reply(&mut menu, &mut mock, &mut seq, json!("yes")).unwrap();
        assert_eq!(mock.applies, 1);
        let next = reply(&mut menu, &mut mock, &mut seq, json!("next")).unwrap();
        assert_eq!(
            next.frames[0]["title"],
            "Native hook profile file (absolute path)"
        );
        assert_eq!(mock.applies, 1);
    }
    fn reply(menu: &mut Menu, mock: &mut Mock, seq: &mut u64, value: Value) -> Result<Outcome> {
        menu.reply(
            &json!({"op":"reply","id":*seq,"value":value}),
            mock,
            &ControlConfig::fresh(&mock.dir).unwrap(),
            seq,
        )
    }
    fn reviewed(menu: &mut Menu, mock: &mut Mock, seq: &mut u64) {
        menu.start(view(), seq).unwrap();
        reply(menu, mock, seq, json!(mock.dir.join("hooks.json"))).unwrap();
        let preset = reply(menu, mock, seq, json!(mock.dir.join("transcript.jsonl"))).unwrap();
        assert_eq!(preset.frames[0]["title"], "Choose hook setup");
        // Delivery and progress are preconfigured; no recipient is inferred.
        let review = reply(menu, mock, seq, json!("defaults")).unwrap();
        assert_eq!(review.frames[0]["mode"], "output");
        assert!(!mock.dir.join("hooks.json").exists());
        reply(menu, mock, seq, json!("close")).unwrap();
    }
    #[test]
    fn reviewed_install_is_single_use_and_cancel_is_side_effect_free() {
        let (mut menu, mut mock, mut seq) = (Menu::default(), Mock::new(), 0);
        reviewed(&mut menu, &mut mock, &mut seq);
        reply(&mut menu, &mut mock, &mut seq, json!("cancel")).unwrap();
        assert_eq!(mock.applies, 0);
        reviewed(&mut menu, &mut mock, &mut seq);
        let id = seq;
        reply(&mut menu, &mut mock, &mut seq, json!("yes")).unwrap();
        assert_eq!(mock.applies, 1);
        assert!(
            menu.reply(
                &json!({"op":"reply","id":id,"value":"yes"}),
                &mut mock,
                &ControlConfig::fresh(std::path::Path::new(crate::test_support::path("/tmp")))
                    .unwrap(),
                &mut seq
            )
            .is_err()
        );
        assert_eq!(mock.applies, 1);
    }
    #[test]
    fn stale_apply_error_consumes_confirmation_without_retry() {
        let (mut menu, mut mock, mut seq) = (Menu::default(), Mock::new(), 0);
        mock.fail = true;
        reviewed(&mut menu, &mut mock, &mut seq);
        assert!(reply(&mut menu, &mut mock, &mut seq, json!("yes")).is_err());
        assert!(!menu.pending());
        assert_eq!(mock.applies, 1);
        assert!(reply(&mut menu, &mut mock, &mut seq, json!("yes")).is_err());
        assert_eq!(mock.applies, 1);
    }
    #[test]
    fn installed_hooks_require_explicit_recipient_selection() {
        let (mut menu, mut mock, mut seq) = (Menu::default(), Mock::new(), 0);
        reviewed(&mut menu, &mut mock, &mut seq);
        let installed = reply(&mut menu, &mut mock, &mut seq, json!("yes")).unwrap();
        assert!(
            installed.frames[0]["options"]
                .as_array()
                .unwrap()
                .iter()
                .any(|v| v[0] == "telegram")
        );
        let outcome = reply(&mut menu, &mut mock, &mut seq, json!("telegram")).unwrap();
        assert!(outcome.message.contains("explicit Read+Send grant"));
        assert!(!menu.pending());
        assert_eq!(mock.applies, 1);
    }
    #[test]
    fn memory_setup_is_reviewed_and_preserves_existing_config() {
        let (mut menu, mut mock, mut seq) = (Menu::default(), Mock::new(), 0);
        reviewed(&mut menu, &mut mock, &mut seq);
        reply(&mut menu, &mut mock, &mut seq, json!("yes")).unwrap();
        reply(&mut menu, &mut mock, &mut seq, json!("memory")).unwrap();
        reply(
            &mut menu,
            &mut mock,
            &mut seq,
            json!("http://127.0.0.1:8888"),
        )
        .unwrap();
        reply(&mut menu, &mut mock, &mut seq, json!("none")).unwrap();
        reply(&mut menu, &mut mock, &mut seq, json!("project-bank")).unwrap();
        assert!(!mock.dir.join("hindsight.json").exists());
        reply(&mut menu, &mut mock, &mut seq, json!("close")).unwrap();
        reply(&mut menu, &mut mock, &mut seq, json!("yes")).unwrap();
        let config = super::super::memory::load(&mock.dir).unwrap().unwrap();
        assert_eq!(config.bindings[0].bank, "project-bank");
        assert!(menu.pending());
        reply(&mut menu, &mut mock, &mut seq, json!("memory")).unwrap();
        reply(&mut menu, &mut mock, &mut seq, json!("replacement-bank")).unwrap();
        reply(&mut menu, &mut mock, &mut seq, json!("close")).unwrap();
        reply(&mut menu, &mut mock, &mut seq, json!("cancel")).unwrap();
        assert_eq!(
            super::super::memory::load(&mock.dir)
                .unwrap()
                .unwrap()
                .bindings[0]
                .bank,
            "project-bank"
        );
    }
    #[test]
    fn herdr_install_requires_confirmation_and_reports_unconfirmed_results() {
        let mut mock = Mock::new();
        let config = ControlConfig::fresh(&mock.dir).unwrap();
        let mut menu = Menu::default();
        let mut seq = 0;
        let review = menu.herdr(&config, "claude", &mut mock, &mut seq).unwrap();
        assert_eq!(review.frames[0]["mode"], "output");
        assert_eq!(mock.applies, 0);
        assert!(reply(&mut menu, &mut mock, &mut seq, json!("yes")).is_err());
        reply(&mut menu, &mut mock, &mut seq, json!("close")).unwrap();
        reply(&mut menu, &mut mock, &mut seq, json!("cancel")).unwrap();
        assert_eq!(mock.applies, 0);
        menu.herdr(&config, "claude", &mut mock, &mut seq).unwrap();
        reply(&mut menu, &mut mock, &mut seq, json!("close")).unwrap();
        mock.fail = true;
        let result = reply(&mut menu, &mut mock, &mut seq, json!("yes")).unwrap();
        assert!(result.refresh && result.message.starts_with("Installation not verified"));
        assert_eq!(mock.applies, 1);
        assert!(reply(&mut menu, &mut mock, &mut seq, json!("yes")).is_err());
        assert_eq!(mock.applies, 1);
    }
    #[test]
    fn removal_review_cancel_and_rollback_use_single_confirmation() {
        struct RemovalSource(super::super::hook_uninstall::tests::Fixture);
        impl Source for RemovalSource {
            fn refresh(&mut self, _: &ControlConfig) -> Result<Observations> {
                Ok(Observations::default())
            }
            fn preview(&mut self, _: &Value, _: usize) -> Result<String> {
                unreachable!()
            }
            fn save_layouts(&mut self, _: &crate::core::layouts::Layouts) -> Result<()> {
                unreachable!()
            }
            fn managed_hooks(
                &mut self,
                harness: &str,
            ) -> Result<Vec<super::super::hook_uninstall::Entry>> {
                super::super::hook_uninstall::list(&self.0.directory, harness)
            }
            fn prepare_hook_removal(
                &mut self,
                path: &std::path::Path,
            ) -> Result<super::super::hook_uninstall::Plan> {
                super::super::hook_uninstall::Plan::prepare(&self.0.directory, path)
            }
            fn apply_hook_removal(
                &mut self,
                plan: super::super::hook_uninstall::Plan,
                approval: &str,
            ) -> Result<PathBuf> {
                plan.apply(approval)
            }
            fn rollback_hook_removal(&mut self, path: &std::path::Path) -> Result<()> {
                super::super::hook_uninstall::rollback(path)
            }
        }
        let mut source = RemovalSource(super::super::hook_uninstall::tests::Fixture::new());
        let before = fs::read(&source.0.profile).unwrap();
        let mut menu = Menu::default();
        let mut seq = 0;
        let config = ControlConfig::fresh(&source.0.directory).unwrap();
        let answer = |menu: &mut Menu, source: &mut RemovalSource, seq: &mut u64, value: &str| {
            menu.reply(
                &json!({"op":"reply","id":*seq,"value":value}),
                source,
                &config,
                seq,
            )
        };
        menu.remove("claude", &mut source, &mut seq).unwrap();
        answer(&mut menu, &mut source, &mut seq, "0").unwrap();
        answer(&mut menu, &mut source, &mut seq, "close").unwrap();
        answer(&mut menu, &mut source, &mut seq, "cancel").unwrap();
        assert_eq!(fs::read(&source.0.profile).unwrap(), before);
        menu.remove("claude", &mut source, &mut seq).unwrap();
        answer(&mut menu, &mut source, &mut seq, "0").unwrap();
        answer(&mut menu, &mut source, &mut seq, "close").unwrap();
        let applied_id = seq;
        answer(&mut menu, &mut source, &mut seq, "yes").unwrap();
        assert!(
            menu.reply(
                &json!({"op":"reply","id":applied_id,"value":"yes"}),
                &mut source,
                &config,
                &mut seq
            )
            .is_err()
        );
        answer(&mut menu, &mut source, &mut seq, "rollback").unwrap();
        assert_eq!(fs::read(&source.0.profile).unwrap(), before);
    }
}
