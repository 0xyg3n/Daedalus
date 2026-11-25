//! Native account actions. The frontend is unchanged; account secrets are never
//! included in process arguments, persisted plans, or result frames.
use super::{
    backend::Source,
    config::ControlConfig,
    session_menu::{Answer, Outcome, Request},
};
use crate::adapters::harnesses;
use serde_json::{Value, json};
type Result<T> = std::result::Result<T, String>;
pub struct Run {
    pub tool: String,
    pub method: Option<String>,
    pub secret: Option<String>,
    pub expected: ControlConfig,
}

pub(crate) fn execute_private_login(
    command: &mut std::process::Command,
    secret: String,
    settings: &super::runtime_settings::Settings,
) -> Result<String> {
    settings.validate()?;
    let mut input = secret.into_bytes();
    input.push(b'\n');
    let result = crate::adapters::process::run_with_input(
        command,
        std::time::Duration::from_millis(settings.native_login_timeout_ms),
        settings.native_login_output_limit_bytes as usize,
        &input,
    );
    input.fill(0);
    let success = match result {
        Ok(mut output) => {
            let success = output.status.success();
            output.stdout.fill(0);
            output.stderr.fill(0);
            success
        }
        Err(_) => false,
    };
    if success {
        Ok("Native sign-in command completed; credentials remain with the harness".into())
    } else {
        Err(
            "Native sign-in outcome is unconfirmed; inspect the harness account before retrying"
                .into(),
        )
    }
}
enum Stage {
    LaunchParent(String, Vec<Value>),
    LaunchReview(Box<super::pane_launch::Plan>),
    GatewayFormat,
    GatewayField(
        usize,
        Option<super::gateway::Config>,
        super::gateway::Config,
    ),
    GatewayEnabled(Option<super::gateway::Config>, super::gateway::Config),
    GatewayConfirm(Box<super::gateway::Plan>),
    Tool(String),
    Method(String),
    Profile(String, Box<ControlConfig>),
    ProfileConfirm(Box<super::setup::SetupPlan>),
    Confirm(Run),
    External(Run),
    Secret(Run),
}
struct Pending {
    id: u64,
    mode: &'static str,
    options: Vec<String>,
    stage: Stage,
}
#[derive(Default)]
pub struct Menu {
    pending: Option<Pending>,
}
pub fn available(action: &str) -> bool {
    matches!(
        action,
        "tools.open" | "tools.signin" | "tools.profile" | "tools.gateway"
    )
}
impl Menu {
    pub fn pending(&self) -> bool {
        self.pending.is_some()
    }
    pub fn cancel(&mut self) {
        self.pending = None;
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
        let options = frame["options"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|v| v[0].as_str().map(str::to_owned))
            .collect();
        self.pending = Some(Pending {
            id: *seq,
            mode,
            options,
            stage,
        });
        Ok(Outcome {
            frames: vec![frame],
            ..Default::default()
        })
    }
    pub fn start(&mut self, action: &str, seq: &mut u64) -> Result<Outcome> {
        if self.pending() {
            return Err("Finish the current account action first".into());
        }
        if action == "tools.gateway" {
            return self.frame(seq,"choose","LLM proxy gateway",json!({"options":[["claude","Claude · Anthropic Messages","Edit the endpoint, model and credential path"],["codex","Codex · OpenAI Responses","Edit the endpoint, model and credential path"],["enable","Enable configured gateway","Review the saved provider override for new launches"],["disable","Disable configured gateway","New launches use their native account again"]],"description":"Gateway traffic uses gateway credentials and its provider billing. Other harnesses keep their native account configuration. Editing settings preserves enable state until you choose otherwise."}),Stage::GatewayFormat);
        }
        let options = harnesses::tools()
            .map(|tool| json!([tool.id, tool.name, tool.profile_variable]))
            .collect::<Vec<_>>();
        self.frame(seq,"choose",match action {"tools.signin"=>"Sign in to an AI tool","tools.profile"=>"Use an existing account profile",_=>"Open an AI tool"},json!({"options":options,"description":"Each tool owns its account. Executable and profile are checked before launch."}),Stage::Tool(action.into()))
    }
    fn execute<S: Source>(
        source: &mut S,
        config: &ControlConfig,
        run: Run,
        external: bool,
    ) -> Result<Outcome> {
        let result = source.session(config, Request::Harness(Box::new(run)));
        let mut out = Outcome {
            refresh: true,
            ..Default::default()
        };
        if external {
            out.frames.push(json!({"version":2,"kind":"external_done"}));
        }
        match result {
            Ok(Answer::Text(text)) => out.message = text,
            Ok(_) => out.frames.push(
                json!({"version":2,"kind":"error","message":"Invalid native account response"}),
            ),
            Err(error) => out
                .frames
                .push(json!({"version":2,"kind":"error","message":error})),
        };
        Ok(out)
    }
    pub fn reply<S: Source>(
        &mut self,
        request: &Value,
        source: &mut S,
        config: &ControlConfig,
        seq: &mut u64,
    ) -> Result<Outcome> {
        let pending = self
            .pending
            .as_ref()
            .ok_or("No account dialog is pending")?;
        if request["op"] != "reply" || request["id"].as_u64() != Some(pending.id) {
            return Err("Unexpected account reply".into());
        }
        let value = request.get("value").ok_or("Missing account reply")?;
        if !value.is_null() {
            let text = value.as_str().ok_or("Invalid account reply")?;
            let valid = match pending.mode {
                "choose" => pending.options.iter().any(|v| v == text),
                "external" => text == "ready",
                "input" => {
                    !text.is_empty() && text.len() <= 8192 && !text.chars().any(char::is_control)
                }
                _ => false,
            };
            if !valid {
                return Err("Invalid account reply".into());
            }
        }
        let pending = self.pending.take().unwrap();
        if value.is_null() {
            return Ok(Outcome::default());
        }
        let text = value.as_str().unwrap();
        match pending.stage {
            Stage::LaunchParent(harness, panes) => {
                let index = text
                    .parse::<usize>()
                    .map_err(|_| "Invalid terminal selection")?;
                let parent = panes
                    .get(index)
                    .ok_or("Terminal selection disappeared")?
                    .clone();
                let cwd = parent
                    .get("cwd")
                    .and_then(Value::as_str)
                    .filter(|s| !s.is_empty())
                    .unwrap_or(&config.agent_home);
                let plan = source.prepare_pane_launch(
                    config,
                    super::pane_launch::Request {
                        harness,
                        parent: parent.clone(),
                        cwd: cwd.into(),
                        model: None,
                    },
                )?;
                let review = super::dialog_text::format(&plan.review())
                    .map_err(|_| "Cannot display launch review")?;
                self.frame(seq,"choose","Start with hooks in a new pane?",json!({"description":review,"options":[["launch","Create pane and start","Hooks register before the native provider starts; sign-in stays with the provider"],["cancel","Cancel","No pane or configuration changes"]]}),Stage::LaunchReview(Box::new(plan)))
            }
            Stage::LaunchReview(plan) => {
                if text == "cancel" {
                    return Ok(Outcome::default());
                }
                if text != "launch" {
                    return Err("Explicit launch confirmation required".into());
                }
                let approval = plan.fingerprint()?;
                let receipt = source.apply_pane_launch(config, *plan, &approval)?;
                Ok(Outcome {
                    message: receipt.message,
                    refresh: true,
                    ..Default::default()
                })
            }
            Stage::GatewayFormat => {
                let Answer::Gateway(expected) = source.session(config, Request::GatewayLoad)?
                else {
                    return Err("Invalid gateway response".into());
                };
                if matches!(text, "enable" | "disable") {
                    let mut updated = expected.clone().ok_or("No gateway is configured")?;
                    updated.enabled = text == "enable";
                    return self.gateway_confirm(source, seq, expected, updated);
                }
                let updated = expected
                    .clone()
                    .unwrap_or(super::gateway::Config::fresh(text)?)
                    .set("provider", text)?;
                self.gateway_field(seq, 0, expected, updated, None)
            }
            Stage::GatewayField(index, expected, updated) => {
                let key = ["endpoint", "model", "credential_file"][index];
                let updated = match updated.set(key, text) {
                    Ok(value) => value,
                    Err(error) => {
                        return self.gateway_field(seq, index, expected, updated, Some(&error));
                    }
                };
                if index < 2 {
                    return self.gateway_field(seq, index + 1, expected, updated, None);
                }
                let options = if updated.enabled {
                    json!([
                        [
                            "true",
                            "Keep enabled",
                            "Preserve gateway use for new launches"
                        ],
                        ["false", "Disable", "Use native accounts for new launches"]
                    ])
                } else {
                    json!([
                        [
                            "false",
                            "Keep disabled",
                            "Save settings without enabling the override"
                        ],
                        [
                            "true",
                            "Enable",
                            "Use this gateway for new matching launches"
                        ]
                    ])
                };
                self.frame(
                    seq,
                    "choose",
                    "Gateway enable state",
                    json!({"options":options}),
                    Stage::GatewayEnabled(expected, updated),
                )
            }
            Stage::GatewayEnabled(expected, updated) => {
                self.gateway_confirm(source, seq, expected, updated.set("enabled", text)?)
            }
            Stage::GatewayConfirm(plan) => {
                if text == "cancel" {
                    return Ok(Outcome::default());
                }
                let message = source.apply_gateway(*plan)?;
                Ok(Outcome {
                    message,
                    refresh: true,
                    ..Default::default()
                })
            }

            Stage::Tool(action) => {
                let tool = harnesses::tool(text).ok_or("Unknown harness")?;
                if action == "tools.profile" {
                    return self.profile_field(seq, text.into(), Box::new(config.clone()), None);
                }
                if action == "tools.signin" {
                    let options = tool
                        .methods
                        .iter()
                        .map(|m| json!([m.id, m.title, m.instruction]))
                        .collect::<Vec<_>>();
                    return self.frame(
                        seq,
                        "choose",
                        &format!("Sign in to {}", tool.name),
                        json!({"options":options}),
                        Stage::Method(text.into()),
                    );
                }
                if cfg!(unix) && matches!(text, "claude" | "pi" | "grok") {
                    let snapshot = source.refresh(config)?.snapshot;
                    let panes = snapshot
                        .get("panes")
                        .and_then(Value::as_array)
                        .into_iter()
                        .flatten()
                        .filter(|p| crate::adapters::terminals::identity(p).is_ok())
                        .cloned()
                        .collect::<Vec<_>>();
                    if panes.is_empty() {
                        return Err(
                            "Create a project terminal first: Workspaces → New project".into()
                        );
                    }
                    let options = panes
                        .iter()
                        .enumerate()
                        .map(|(i, p)| {
                            json!([
                                i.to_string(),
                                format!(
                                    "{} · {}",
                                    p.get("session")
                                        .and_then(Value::as_str)
                                        .unwrap_or("workspace"),
                                    p.get("pane_id")
                                        .and_then(Value::as_str)
                                        .unwrap_or("terminal")
                                ),
                                p.get("cwd")
                                    .and_then(Value::as_str)
                                    .unwrap_or(&config.agent_home)
                            ])
                        })
                        .collect::<Vec<_>>();
                    return self.frame(seq,"choose","Create a new pane beside which terminal?",json!({"options":options,"description":"Uses the configured native account profile and this terminal's project directory."}),Stage::LaunchParent(text.into(),panes));
                }
                self.confirm(
                    seq,
                    Run {
                        tool: text.into(),
                        method: None,
                        secret: None,
                        expected: config.clone(),
                    },
                )
            }
            Stage::Method(tool) => self.confirm(
                seq,
                Run {
                    tool,
                    method: Some(text.into()),
                    secret: None,
                    expected: config.clone(),
                },
            ),
            Stage::Profile(tool, expected) => {
                let key = format!("profiles.{tool}");
                let updated = match expected.with_setting(&key, text) {
                    Ok(updated) => updated,
                    Err(error) => return self.profile_field(seq, tool, expected, Some(&error)),
                };
                let before = expected.setting_value(&key)?;
                let after = updated.setting_value(&key)?;
                let plan = source.prepare_configuration(&expected, updated)?;
                let description=super::dialog_text::format(&json!({"tool":tool,"previous_saved_profile":before,"saved_profile":after,"configuration_file":plan.preview.destination,"backup_required":plan.preview.backup_required,"inheritance":"A null saved profile uses the environment or tool default. Changes apply to future launches; native account data is retained."})).map_err(|_|"Cannot format profile review")?;
                self.frame(seq,"choose","Save native account profile?",json!({"description":description,"options":[["cancel","Cancel","Keep the saved profile"],["yes","Save","Save exactly the reviewed configuration with backup"]]}),Stage::ProfileConfirm(Box::new(plan)))
            }
            Stage::ProfileConfirm(plan) => {
                if text == "cancel" {
                    return Ok(Outcome::default());
                }
                let configuration = source.apply_configuration(*plan)?;
                Ok(Outcome {
                    configuration: Some(configuration),
                    message: "Native profile selection saved".into(),
                    refresh: true,
                    ..Default::default()
                })
            }
            Stage::Confirm(run) => {
                if text == "cancel" {
                    return Ok(Outcome::default());
                }
                let secret = run
                    .method
                    .as_ref()
                    .and_then(|id| {
                        harnesses::tool(&run.tool).and_then(|tool| {
                            tool.methods
                                .iter()
                                .find(|m| m.id == id)
                                .map(|m| m.secret_stdin)
                        })
                    })
                    .unwrap_or(false);
                if secret {
                    self.frame(
                        seq,
                        "input",
                        "Account API key (hidden)",
                        json!({"secret":true}),
                        Stage::Secret(run),
                    )
                } else {
                    self.frame(
                        seq,
                        "external",
                        "Open native tool",
                        json!({}),
                        Stage::External(run),
                    )
                }
            }
            Stage::Secret(mut run) => {
                run.secret = Some(text.into());
                Self::execute(source, config, run, false)
            }
            Stage::External(run) => Self::execute(source, config, run, true),
        }
    }
    fn gateway_field(
        &mut self,
        seq: &mut u64,
        index: usize,
        expected: Option<super::gateway::Config>,
        updated: super::gateway::Config,
        error: Option<&str>,
    ) -> Result<Outcome> {
        let (title, input) = match index {
            0 => (
                "Gateway base URL (HTTPS, or local loopback HTTP)",
                updated.endpoint.clone(),
            ),
            1 => ("Gateway model ID", updated.model.clone()),
            _ => (
                "Existing private credential file (absolute path)",
                updated.credential_file.to_string_lossy().into_owned(),
            ),
        };
        self.frame(seq,"input",title,json!({"input":input,"secret":false,"description":error.unwrap_or("Enter a value; Ctrl+U clears the field. Credential entry accepts a file path, never a token.")}),Stage::GatewayField(index,expected,updated))
    }
    fn profile_field(
        &mut self,
        seq: &mut u64,
        tool: String,
        expected: Box<ControlConfig>,
        error: Option<&str>,
    ) -> Result<Outcome> {
        let native = harnesses::tool(&tool).ok_or("Unknown native tool")?;
        let input = expected
            .profiles
            .get(&tool)
            .map(String::as_str)
            .unwrap_or("inherit");
        let description = format!(
            "Profile root for {} ({}). Enter an existing absolute directory, or inherit to clear the saved override and use the environment/tool default. Ctrl+U clears the field.{}",
            native.name,
            native.profile_variable,
            error.map(|e| format!("\n{e}")).unwrap_or_default()
        );
        self.frame(
            seq,
            "input",
            "Existing native profile directory",
            json!({"secret":false,"input":input,"description":description}),
            Stage::Profile(tool, expected),
        )
    }
    fn gateway_confirm<S: Source>(
        &mut self,
        source: &mut S,
        seq: &mut u64,
        expected: Option<super::gateway::Config>,
        updated: super::gateway::Config,
    ) -> Result<Outcome> {
        let plan = source.prepare_gateway(expected, updated)?;
        let description = format!(
            "Applies only to newly launched matching harnesses; native login remains separate.\n{}",
            super::dialog_text::format(&plan.review())
                .map_err(|_| "Cannot format gateway review")?
        );
        self.frame(seq,"choose","Confirm gateway configuration",json!({"description":description,"options":[["cancel","Cancel","Keep current gateway configuration"],["yes","Continue","Save reviewed private gateway configuration with backup"]]}),Stage::GatewayConfirm(Box::new(plan)))
    }
    fn confirm(&mut self, seq: &mut u64, run: Run) -> Result<Outcome> {
        let label = if run.method.is_some() {
            "Run the native sign-in command"
        } else {
            "Open this native harness"
        };
        self.frame(seq,"choose","Confirm action",json!({"description":format!("{label}: {}. It uses the configured account directory. Native account files may change.",run.tool),"options":[["cancel","Cancel","Leave everything unchanged"],["yes","Continue",label]]}),Stage::Confirm(run))
    }
}
/// Build the reviewed native operation; no subprocess or account read occurs here.
pub fn plan(
    run: &Run,
    config: &ControlConfig,
    environment: &std::collections::BTreeMap<String, String>,
) -> Result<harnesses::ProcessPlan> {
    if &run.expected != config {
        return Err("Configuration changed; review the account action again".into());
    }
    let tool = harnesses::tool(&run.tool).ok_or("Unknown harness")?;
    let binary =
        harnesses::executable(&tool, environment).ok_or("Install the selected tool first")?;
    if let Some(method) = &run.method {
        harnesses::login_plan(
            &tool,
            method,
            &binary,
            std::path::Path::new(&config.agent_home),
            environment,
            &config.profiles,
        )
    } else {
        harnesses::launch_plan(
            &tool,
            &binary,
            std::path::Path::new(&config.agent_home),
            environment,
            &config.profiles,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::super::backend::Observations;
    use super::*;
    #[derive(Default)]
    struct Mock {
        calls: usize,
        secret: Option<String>,
        fail: bool,
        gateway: Option<super::super::gateway::Config>,
        gateway_directory: Option<std::path::PathBuf>,
        configuration_directory: Option<std::path::PathBuf>,
        configured: Option<ControlConfig>,
    }
    impl Source for Mock {
        fn prepare_configuration(
            &mut self,
            expected: &ControlConfig,
            updated: ControlConfig,
        ) -> Result<super::super::setup::SetupPlan> {
            super::super::setup::SetupPlan::edit_if_current(
                self.configuration_directory
                    .as_ref()
                    .expect("private configuration fixture"),
                expected,
                updated,
            )
        }
        fn apply_configuration(
            &mut self,
            plan: super::super::setup::SetupPlan,
        ) -> Result<ControlConfig> {
            let updated = plan.config().clone();
            plan.apply()?;
            self.configured = Some(updated.clone());
            self.calls += 1;
            Ok(updated)
        }
        fn prepare_gateway(
            &mut self,
            expected: Option<super::super::gateway::Config>,
            updated: super::super::gateway::Config,
        ) -> Result<super::super::gateway::Plan> {
            super::super::gateway::Plan::prepare(
                self.gateway_directory
                    .as_ref()
                    .expect("private gateway fixture"),
                &expected,
                updated,
            )
        }
        fn apply_gateway(&mut self, plan: super::super::gateway::Plan) -> Result<String> {
            let approval = plan.approval();
            plan.apply(&approval)?;
            self.gateway = super::super::gateway::load(self.gateway_directory.as_ref().unwrap())?;
            self.calls += 1;
            Ok("Gateway configuration saved".into())
        }
        fn refresh(&mut self, _: &ControlConfig) -> Result<Observations> {
            Ok(Observations {
                snapshot: json!({"panes":[{"backend":"herdr","session":"fixture","pane_id":"pane","terminal_id":"generation","cwd":crate::test_support::path("/tmp/project")}]}),
                ..Observations::default()
            })
        }
        fn prepare_pane_launch(
            &mut self,
            config: &ControlConfig,
            request: super::super::pane_launch::Request,
        ) -> Result<super::super::pane_launch::Plan> {
            let executable = json!({"device":1,"inode":1,"size":1,"modified":1,"modified_ns":1,"changed":1,"changed_ns":1});
            serde_json::from_value(json!({"request":request,"configuration":config,"environment":{},"profile":crate::test_support::path("/tmp/profile"),"hook":{"spec":{"registration":{"version":1,"id":"fixture","directory":crate::test_support::path("/tmp/state"),"scope":{"instance":"agent","terminal":{"backend":"herdr","endpoint":"fixture","pane":"pane","generation":"generation"},"harness":"claude","conversation":"12345678-abcd-4000-8000-123456789abc"},"goal_id":null,"harness_process":null,"transcript":null,"events":["SessionStart"],"options":{"telegram_delivery":false,"telegram_progress":false,"notifications":false,"hindsight":false},"activity_lease_ms":60000,"pending_timeout_ms":30000,"hook_timeout_ms":10000},"native_binary":crate::test_support::path("/tmp/provider"),"daedalus_binary":crate::test_support::path("/tmp/daedalus"),"working_directory":crate::test_support::path("/tmp/project"),"observed_version":"2.1.295","expires_ms":1,"model":null},"native":executable,"daemon":executable,"hooks":[]}})).map_err(|e|e.to_string())
        }
        fn apply_pane_launch(
            &mut self,
            _: &ControlConfig,
            _: super::super::pane_launch::Plan,
            _: &str,
        ) -> Result<crate::adapters::terminal_actions::Receipt> {
            self.calls += 1;
            Ok(crate::adapters::terminal_actions::Receipt {
                state: crate::adapters::terminal_actions::ReceiptState::Submitted,
                message: "Submitted; readiness unknown".into(),
                terminal: None,
            })
        }

        fn preview(&mut self, _: &Value, _: usize) -> Result<String> {
            unreachable!()
        }
        fn save_layouts(&mut self, _: &crate::core::layouts::Layouts) -> Result<()> {
            unreachable!()
        }
        fn session(&mut self, _: &ControlConfig, request: Request) -> Result<Answer> {
            if let Request::Configure { expected, updated } = &request {
                assert_eq!(expected.as_ref(), &config());
                self.configured = Some(updated.as_ref().clone());
                self.calls += 1;
                return Ok(Answer::Configured {
                    configuration: updated.clone(),
                    message: "Saved privately".into(),
                });
            }
            if matches!(&request, Request::GatewayLoad) {
                return Ok(Answer::Gateway(self.gateway.clone()));
            }
            let Request::Harness(run) = request else {
                panic!("Unexpected mutation")
            };
            self.calls += 1;
            self.secret = run.secret;
            if self.fail {
                Err("Native outcome unconfirmed".into())
            } else {
                Ok(Answer::Text("completed".into()))
            }
        }
    }
    fn config() -> ControlConfig {
        ControlConfig::fresh(std::path::Path::new(crate::test_support::path(
            "/tmp/project",
        )))
        .unwrap()
    }
    fn profile_fixture() -> (std::path::PathBuf, Mock) {
        let root = std::env::temp_dir().join(format!(
            "daedalus-profile-menu-{}-{}",
            std::process::id(),
            crate::test_support::unique_nanos()
        ));
        let directory = root.join("state");
        crate::core::layouts::write_private(
            &directory.join("control.json"),
            &config().encode().unwrap(),
        )
        .unwrap();
        (
            root,
            Mock {
                configuration_directory: Some(directory),
                ..Default::default()
            },
        )
    }
    fn reply(menu: &mut Menu, mock: &mut Mock, seq: &mut u64, value: Value) -> Outcome {
        let config = mock.configured.clone().unwrap_or_else(config);
        menu.reply(
            &json!({"op":"reply","id":*seq,"value":value}),
            mock,
            &config,
            seq,
        )
        .unwrap()
    }
    #[test]
    fn open_waits_for_ready_restores_after_failure_and_rejects_replay() {
        let (mut menu, mut mock, mut seq) = (
            Menu::default(),
            Mock {
                fail: true,
                ..Default::default()
            },
            0,
        );
        menu.start("tools.open", &mut seq).unwrap();
        reply(&mut menu, &mut mock, &mut seq, json!("codex"));
        let out = reply(&mut menu, &mut mock, &mut seq, json!("yes"));
        assert_eq!(out.frames[0]["mode"], "external");
        assert_eq!(mock.calls, 0);
        let id = seq;
        let out = reply(&mut menu, &mut mock, &mut seq, json!("ready"));
        assert_eq!(out.frames[0]["kind"], "external_done");
        assert_eq!(out.frames[1]["kind"], "error");
        assert_eq!(mock.calls, 1);
        assert!(
            menu.reply(
                &json!({"op":"reply","id":id,"value":"ready"}),
                &mut mock,
                &config(),
                &mut seq
            )
            .is_err()
        );
        assert_eq!(mock.calls, 1);
    }
    #[cfg(unix)]
    #[test]
    fn guided_launch_reviews_parent_and_cancellation_or_replay_never_spawns_twice() {
        for final_reply in ["cancel", "launch"] {
            let (mut menu, mut mock, mut seq) = (Menu::default(), Mock::default(), 0);
            menu.start("tools.open", &mut seq).unwrap();
            let selected = reply(&mut menu, &mut mock, &mut seq, json!("claude"));
            assert_eq!(selected.frames[0]["mode"], "choose");
            let review = reply(&mut menu, &mut mock, &mut seq, json!("0"));
            assert!(
                review.frames[0]["description"]
                    .as_str()
                    .unwrap()
                    .contains("native profile")
            );
            assert_eq!(mock.calls, 0);
            let id = seq;
            reply(&mut menu, &mut mock, &mut seq, json!(final_reply));
            assert_eq!(mock.calls, usize::from(final_reply == "launch"));
            assert!(
                menu.reply(
                    &json!({"op":"reply","id":id,"value":"launch"}),
                    &mut mock,
                    &config(),
                    &mut seq
                )
                .is_err()
            );
        }
    }
    #[test]
    fn secret_login_is_private_and_cancel_does_not_execute() {
        let (mut menu, mut mock, mut seq) = (Menu::default(), Mock::default(), 0);
        menu.start("tools.signin", &mut seq).unwrap();
        reply(&mut menu, &mut mock, &mut seq, json!("codex"));
        reply(&mut menu, &mut mock, &mut seq, json!("api-key"));
        reply(&mut menu, &mut mock, &mut seq, json!("cancel"));
        assert_eq!(mock.calls, 0);
        menu.start("tools.signin", &mut seq).unwrap();
        reply(&mut menu, &mut mock, &mut seq, json!("codex"));
        reply(&mut menu, &mut mock, &mut seq, json!("api-key"));
        let out = reply(&mut menu, &mut mock, &mut seq, json!("yes"));
        assert_eq!(out.frames[0]["secret"], true);
        let out = reply(&mut menu, &mut mock, &mut seq, json!("PRIVATE-ACCOUNT-KEY"));
        assert_eq!(mock.secret.as_deref(), Some("PRIVATE-ACCOUNT-KEY"));
        assert!(!format!("{out:?}").contains("PRIVATE"));
    }
    #[test]
    fn changed_configuration_is_rejected_before_binary_lookup() {
        let run = Run {
            tool: "claude".into(),
            method: None,
            secret: None,
            expected: config(),
        };
        let mut changed = config();
        changed.agent_name = "changed".into();
        assert!(
            plan(&run, &changed, &Default::default())
                .unwrap_err()
                .contains("Configuration changed")
        );
    }
    #[test]
    fn gateway_configuration_requires_review_and_cancellation_keeps_old_state() {
        let (mut menu, mut mock, mut seq) = (Menu::default(), Mock::default(), 0);
        let root = std::env::temp_dir().join(format!(
            "daedalus-gateway-menu-{}-{}",
            std::process::id(),
            crate::test_support::unique_nanos()
        ));
        let token = root.join("token");
        crate::core::layouts::write_private(&token, b"fixture-raw-token").unwrap();
        mock.gateway_directory = Some(root.clone());
        for final_reply in ["cancel", "yes"] {
            menu.start("tools.gateway", &mut seq).unwrap();
            reply(&mut menu, &mut mock, &mut seq, json!("claude"));
            reply(
                &mut menu,
                &mut mock,
                &mut seq,
                json!("https://gateway.example.invalid"),
            );
            reply(&mut menu, &mut mock, &mut seq, json!("model"));
            reply(&mut menu, &mut mock, &mut seq, json!(token));
            let review = reply(&mut menu, &mut mock, &mut seq, json!("false"));
            assert!(
                review.frames[0]["description"]
                    .as_str()
                    .unwrap()
                    .contains("newly launched")
            );
            assert_eq!(mock.calls, 0);
            reply(&mut menu, &mut mock, &mut seq, json!(final_reply));
        }
        assert_eq!(mock.calls, 1);
        assert!(!mock.gateway.as_ref().unwrap().enabled);
        menu.start("tools.gateway", &mut seq).unwrap();
        reply(&mut menu, &mut mock, &mut seq, json!("enable"));
        reply(&mut menu, &mut mock, &mut seq, json!("yes"));
        assert!(mock.gateway.as_ref().unwrap().enabled);
        menu.start("tools.gateway", &mut seq).unwrap();
        reply(&mut menu, &mut mock, &mut seq, json!("disable"));
        reply(&mut menu, &mut mock, &mut seq, json!("yes"));
        assert!(!mock.gateway.as_ref().unwrap().enabled);
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn profile_path_entry_waits_for_explicit_save_review() {
        let (root, mut mock) = profile_fixture();
        let path = root.join("profile");
        std::fs::create_dir(&path).unwrap();
        let (mut menu, mut seq) = (Menu::default(), 0);
        menu.start("tools.profile", &mut seq).unwrap();
        reply(&mut menu, &mut mock, &mut seq, json!("claude"));
        let out = reply(&mut menu, &mut mock, &mut seq, json!(path));
        let calls = mock.calls;
        reply(&mut menu, &mut mock, &mut seq, json!("cancel"));
        assert_eq!(
            std::fs::read(root.join("state/control.json")).unwrap(),
            config().encode().unwrap()
        );
        std::fs::remove_dir_all(&root).unwrap();
        assert_eq!(calls, 0, "Profile entry applied before a save review");
        assert_eq!(out.frames[0]["title"], "Save native account profile?");
    }
    #[test]
    fn profile_selection_updates_only_selected_harness_without_touching_profile() {
        let (root, mut mock) = profile_fixture();
        let path = root.join("profile");
        std::fs::create_dir(&path).unwrap();
        std::fs::write(path.join("native-account"), b"fixture-unchanged").unwrap();
        let (mut menu, mut seq) = (Menu::default(), 0);
        menu.start("tools.profile", &mut seq).unwrap();
        reply(&mut menu, &mut mock, &mut seq, json!("claude"));
        reply(&mut menu, &mut mock, &mut seq, json!(path));
        let out = reply(&mut menu, &mut mock, &mut seq, json!("yes"));
        assert_eq!(mock.calls, 1);
        assert_eq!(
            out.configuration.unwrap().profiles["claude"],
            path.to_string_lossy()
        );
        assert_eq!(
            std::fs::read(path.join("native-account")).unwrap(),
            b"fixture-unchanged"
        );
        menu.start("tools.profile", &mut seq).unwrap();
        let field = reply(&mut menu, &mut mock, &mut seq, json!("claude"));
        assert_eq!(field.frames[0]["input"], json!(path));
        let invalid = reply(&mut menu, &mut mock, &mut seq, json!("relative"));
        assert!(
            invalid.frames[0]["description"]
                .as_str()
                .unwrap()
                .contains("absolute")
        );
        assert_eq!(invalid.frames[0]["input"], json!(path));
        reply(&mut menu, &mut mock, &mut seq, json!("inherit"));
        let out = reply(&mut menu, &mut mock, &mut seq, json!("yes"));
        assert!(!out.configuration.unwrap().profiles.contains_key("claude"));
        assert_eq!(
            std::fs::read(path.join("native-account")).unwrap(),
            b"fixture-unchanged"
        );
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn profile_review_refuses_changed_bytes_and_cannot_be_replayed() {
        let (root, mut mock) = profile_fixture();
        let (mut menu, mut seq) = (Menu::default(), 0);
        menu.start("tools.profile", &mut seq).unwrap();
        reply(&mut menu, &mut mock, &mut seq, json!("codex"));
        reply(&mut menu, &mut mock, &mut seq, json!(root));
        let mut changed = config();
        changed.ui.colors = false;
        crate::core::layouts::write_private(
            &root.join("state/control.json"),
            &changed.encode().unwrap(),
        )
        .unwrap();
        let answer = json!({"op":"reply","id":seq,"value":"yes"});
        assert!(menu.reply(&answer, &mut mock, &config(), &mut seq).is_err());
        assert!(menu.reply(&answer, &mut mock, &config(), &mut seq).is_err());
        assert_eq!(mock.calls, 0);
        assert_eq!(
            std::fs::read(root.join("state/control.json")).unwrap(),
            changed.encode().unwrap()
        );
        std::fs::remove_dir_all(root).unwrap();
    }
    #[cfg(unix)]
    #[test]
    fn private_login_uses_saved_limits_and_discards_native_output_without_retry() {
        use super::super::runtime_settings::Settings;
        let (root, _mock) = profile_fixture();
        let marker = root.join("attempts");
        let secret = "PRIVATE-STDIN-FIXTURE";
        let run = |script: &str, settings: &Settings| {
            let mut command = std::process::Command::new("/bin/sh");
            command
                .env_clear()
                .args(["-c", script, "private-login-fixture"])
                .arg(&marker);
            let result = execute_private_login(&mut command, secret.into(), settings);
            let text = result.as_ref().unwrap_or_else(|error| error);
            assert!(!text.contains(secret));
            result
        };
        let default = Settings::default();
        assert!(run("printf x >> \"$1\"; read -r private; printf '%s' \"$private\"; printf '%s' \"$private\" >&2",&default).is_ok());
        assert!(
            run(
                "printf x >> \"$1\"; read -r private; printf '%s' \"$private\" >&2; exit 1",
                &default
            )
            .is_err()
        );
        let output_script = "printf x >> \"$1\"; read -r private; i=0; while [ \"$i\" -lt 128 ]; do printf 'abcdefghijklmnop'; i=$((i+1)); done";
        let low = Settings {
            native_login_output_limit_bytes: 1024,
            ..default.clone()
        };
        assert!(run(output_script, &low).is_err());
        assert!(run(output_script, &default).is_ok());
        let timeout = Settings {
            native_login_timeout_ms: 100,
            ..default.clone()
        };
        let sleeping = "printf x >> \"$1\"; read -r private; exec /bin/sleep 1";
        assert!(run(sleeping, &timeout).is_err());
        assert!(
            run(
                sleeping,
                &Settings {
                    native_login_timeout_ms: 1500,
                    ..default
                }
            )
            .is_ok()
        );
        assert_eq!(
            std::fs::read(&marker).unwrap(),
            b"xxxxxx",
            "A native command was retried"
        );
        std::fs::remove_dir_all(root).unwrap();
    }
}
