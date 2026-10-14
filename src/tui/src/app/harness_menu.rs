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
enum Stage {
    LaunchParent(String, Vec<Value>),
    LaunchReview(Box<super::pane_launch::Plan>),
    GatewayFormat,
    GatewayField(
        usize,
        Option<super::gateway::Config>,
        super::gateway::Config,
    ),
    GatewayConfirm(Option<super::gateway::Config>, super::gateway::Config),
    Tool(String),
    Method(String),
    Profile(String),
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
            return self.frame(seq,"choose","LLM proxy gateway",json!({"options":[["claude","Claude · Anthropic Messages","Existing gateway exposing the documented Anthropic API"],["codex","Codex · OpenAI Responses","Existing gateway exposing the documented Responses API"],["disable","Disable configured gateway","New launches use their native account again"]],"description":"Gateway traffic uses gateway credentials and its provider billing. Other harnesses keep their native account configuration."}),Stage::GatewayFormat);
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
        let result = source.session(config, Request::Harness(run));
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
                if text == "disable" {
                    let mut updated = expected.clone().ok_or("No gateway is configured")?;
                    updated.enabled = false;
                    return self.gateway_confirm(seq, expected, updated);
                }
                let updated = super::gateway::Config {
                    version: 1,
                    enabled: true,
                    format: if text == "claude" {
                        super::gateway::Format::AnthropicMessages
                    } else {
                        super::gateway::Format::OpenaiResponses
                    },
                    endpoint: String::new(),
                    model: String::new(),
                    credential_file: std::path::PathBuf::new(),
                    harness: text.into(),
                };
                self.frame(
                    seq,
                    "input",
                    "Gateway base URL (HTTPS, or local loopback HTTP)",
                    json!({"secret":false}),
                    Stage::GatewayField(0, expected, updated),
                )
            }
            Stage::GatewayField(index, expected, mut updated) => match index {
                0 => {
                    updated.endpoint = text.into();
                    self.frame(
                        seq,
                        "input",
                        "Gateway model ID",
                        json!({"secret":false}),
                        Stage::GatewayField(1, expected, updated),
                    )
                }
                1 => {
                    updated.model = text.into();
                    self.frame(
                        seq,
                        "input",
                        "Existing private credential file (absolute path)",
                        json!({"secret":false}),
                        Stage::GatewayField(2, expected, updated),
                    )
                }
                _ => {
                    updated.credential_file = text.into();
                    updated.validate()?;
                    self.gateway_confirm(seq, expected, updated)
                }
            },
            Stage::GatewayConfirm(expected, updated) => {
                if text == "cancel" {
                    return Ok(Outcome::default());
                }
                let Answer::Text(message) =
                    source.session(config, Request::GatewaySave { expected, updated })?
                else {
                    return Err("Invalid gateway save response".into());
                };
                Ok(Outcome {
                    message,
                    refresh: true,
                    ..Default::default()
                })
            }

            Stage::Tool(action) => {
                let tool = harnesses::tool(text).ok_or("Unknown harness")?;
                if action == "tools.profile" {
                    return self.frame(
                        seq,
                        "input",
                        "Existing native profile directory",
                        json!({"secret":false}),
                        Stage::Profile(text.into()),
                    );
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
                if matches!(text, "claude" | "pi" | "grok") {
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
                        return Err("Create a tmux or Herdr project terminal first".into());
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
            Stage::Profile(tool) => {
                let path = std::path::Path::new(text);
                crate::core::layouts::check_path(path)?;
                if !path.is_dir() {
                    return Err("Profile directory does not exist".into());
                }
                let mut updated = config.clone();
                updated.profiles.insert(tool, text.into());
                updated.validate()?;
                let Answer::Configured {
                    configuration,
                    message,
                } = source.session(
                    config,
                    Request::Configure {
                        expected: Box::new(config.clone()),
                        updated: Box::new(updated),
                    },
                )?
                else {
                    return Err("Invalid profile configuration response".into());
                };
                Ok(Outcome {
                    configuration: Some(configuration),
                    message,
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
    fn gateway_confirm(
        &mut self,
        seq: &mut u64,
        expected: Option<super::gateway::Config>,
        updated: super::gateway::Config,
    ) -> Result<Outcome> {
        let description = format!(
            "{} gateway for {} · endpoint {} · model {} · credential file {}. Applies only to newly launched matching harnesses; native login remains separate.",
            if updated.enabled { "Enable" } else { "Disable" },
            updated.harness,
            updated.endpoint,
            updated.model,
            updated.credential_file.display()
        );
        self.frame(seq,"choose","Confirm action",json!({"description":description,"options":[["cancel","Cancel","Keep current gateway configuration"],["yes","Continue","Save private gateway configuration"]]}),Stage::GatewayConfirm(expected,updated))
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
        configured: Option<ControlConfig>,
    }
    impl Source for Mock {
        fn refresh(&mut self, _: &ControlConfig) -> Result<Observations> {
            Ok(Observations {
                snapshot: json!({"panes":[{"backend":"herdr","session":"fixture","pane_id":"pane","terminal_id":"generation","cwd":"/tmp/project"}]}),
                ..Observations::default()
            })
        }
        fn prepare_pane_launch(
            &mut self,
            config: &ControlConfig,
            request: super::super::pane_launch::Request,
        ) -> Result<super::super::pane_launch::Plan> {
            let executable = json!({"device":1,"inode":1,"size":1,"modified":1,"modified_ns":1,"changed":1,"changed_ns":1});
            serde_json::from_value(json!({"request":request,"configuration":config,"environment":{},"profile":"/tmp/profile","hook":{"spec":{"registration":{"version":1,"id":"fixture","directory":"/tmp/state","scope":{"instance":"agent","terminal":{"backend":"herdr","endpoint":"fixture","pane":"pane","generation":"generation"},"harness":"claude","conversation":"12345678-abcd-4000-8000-123456789abc"},"goal_id":null,"harness_process":null,"transcript":null,"events":["SessionStart"],"options":{"telegram_delivery":false,"telegram_progress":false,"notifications":false,"hindsight":false},"activity_lease_ms":60000,"pending_timeout_ms":30000,"hook_timeout_ms":10000},"native_binary":"/tmp/provider","daedalus_binary":"/tmp/daedalus","working_directory":"/tmp/project","observed_version":"2.1.295","expires_ms":1,"model":null},"native":executable,"daemon":executable,"hooks":[]}})).map_err(|e|e.to_string())
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
                    configuration: updated.as_ref().clone(),
                    message: "Saved privately".into(),
                });
            }
            if matches!(&request, Request::GatewayLoad) {
                return Ok(Answer::Gateway(self.gateway.clone()));
            }
            if let Request::GatewaySave { expected, updated } = &request {
                assert_eq!(&self.gateway, expected);
                self.gateway = Some(updated.clone());
                self.calls += 1;
                return Ok(Answer::Text("Saved".into()));
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
        ControlConfig::fresh(std::path::Path::new("/tmp/project")).unwrap()
    }
    fn reply(menu: &mut Menu, mock: &mut Mock, seq: &mut u64, value: Value) -> Outcome {
        menu.reply(
            &json!({"op":"reply","id":*seq,"value":value}),
            mock,
            &config(),
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
            let review = reply(
                &mut menu,
                &mut mock,
                &mut seq,
                json!("/tmp/private-gateway-token"),
            );
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
        assert!(mock.gateway.as_ref().unwrap().enabled);
        menu.start("tools.gateway", &mut seq).unwrap();
        reply(&mut menu, &mut mock, &mut seq, json!("disable"));
        reply(&mut menu, &mut mock, &mut seq, json!("yes"));
        assert!(!mock.gateway.as_ref().unwrap().enabled);
    }
    #[test]
    fn profile_selection_updates_only_selected_harness_without_touching_profile() {
        let path =
            std::env::temp_dir().join(format!("daedalus-native-profile-{}", std::process::id()));
        std::fs::create_dir(&path).unwrap();
        std::fs::write(path.join("native-account"), b"fixture-unchanged").unwrap();
        let (mut menu, mut mock, mut seq) = (Menu::default(), Mock::default(), 0);
        menu.start("tools.profile", &mut seq).unwrap();
        reply(&mut menu, &mut mock, &mut seq, json!("claude"));
        let out = reply(&mut menu, &mut mock, &mut seq, json!(path));
        assert_eq!(mock.calls, 1);
        assert_eq!(
            out.configuration.unwrap().profiles["claude"],
            path.to_string_lossy()
        );
        assert_eq!(
            std::fs::read(path.join("native-account")).unwrap(),
            b"fixture-unchanged"
        );
        std::fs::remove_dir_all(path).unwrap();
    }
}
