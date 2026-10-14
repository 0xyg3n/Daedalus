//! Typed workflow editing and sequential execution for the unchanged TUI protocol.
use super::{
    backend::Source,
    config::ControlConfig,
    session_actions::AttachMode,
    session_menu::{Answer, Outcome, Request, View, reference},
    tree,
};
use crate::core::layouts::{self, AttachMode as SavedAttachMode, Step, Workflow};
use serde_json::{Value, json};
type Result<T> = std::result::Result<T, String>;
enum Stage {
    RouteChat(crate::core::layouts::TerminalRef),
    RouteGrant {
        index: usize,
        target: Box<crate::core::routes::Terminal>,
        grants: Vec<(i64, i64)>,
    },
    RouteConfirm {
        index: usize,
        plan: Box<super::telegram_admin::Plan>,
    },
    Name,
    Actions,
    Edit,
    Kind,
    Layout,
    Terminal(String, Vec<Value>),
    Mode(Value),
    Delete,
    Run,
    Output,
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
    view: Option<View>,
    name: String,
    steps: Vec<Step>,
    route_plans: std::collections::BTreeMap<usize, Box<super::telegram_admin::Plan>>,
    run_index: usize,
    run_results: Vec<String>,
    run_layouts: Option<layouts::Layouts>,
}
impl Menu {
    pub fn pending(&self) -> bool {
        self.pending.is_some()
    }
    pub fn cancel(&mut self) {
        self.pending = None;
        self.view = None;
        self.route_plans.clear();
        self.run_layouts = None;
        self.run_results.clear();
        self.run_index = 0;
        self.steps.clear();
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
            .filter_map(|r| r[0].as_str().map(str::to_owned))
            .collect();
        self.pending = Some(Pending {
            id: *seq,
            mode,
            options,
            stage,
        });
        Ok(Outcome {
            frames: vec![frame],
            ..Outcome::default()
        })
    }
    fn choose(
        &mut self,
        seq: &mut u64,
        title: &str,
        options: Value,
        description: &str,
        stage: Stage,
    ) -> Result<Outcome> {
        self.frame(
            seq,
            "choose",
            title,
            json!({"options":options,"description":description}),
            stage,
        )
    }
    fn edit(&mut self, seq: &mut u64) -> Result<Outcome> {
        let mut options = self
            .steps
            .iter()
            .enumerate()
            .map(|(i, step)| {
                json!([
                    i.to_string(),
                    format!("{}. {}", i + 1, describe(step)),
                    "Enter removes this step"
                ])
            })
            .collect::<Vec<_>>();
        options.extend([
            json!([
                "add",
                "Add a step…",
                "Typed actions only; no shell commands or credentials."
            ]),
            json!([
                "save",
                "Save workflow",
                format!("{} steps", self.steps.len())
            ]),
        ]);
        self.choose(
            seq,
            &format!("Workflow “{}”", self.name),
            json!(options),
            "Steps run in order when you choose Run; route changes ask first.",
            Stage::Edit,
        )
    }
    pub fn start(&mut self, action: &str, view: View, seq: &mut u64) -> Result<Outcome> {
        if self.pending() {
            return Err("Finish the current workflow dialog first".into());
        }
        self.view = Some(view);
        if action == "workflow.new" {
            self.steps.clear();
            return self.frame(
                seq,
                "input",
                "Name this workflow",
                json!({"secret":false}),
                Stage::Name,
            );
        }
        self.name = action
            .strip_prefix("workflow:")
            .ok_or("Unknown workflow action")?
            .into();
        self.steps = self
            .view
            .as_ref()
            .unwrap()
            .layouts
            .workflows
            .get(&self.name)
            .ok_or("Workflow changed; select it again")?
            .steps
            .clone();
        self.choose(
            seq,
            &format!("Workflow “{}”", self.name),
            json!([
                ["run", "Run", "Review the steps, then run them in order."],
                ["edit", "Edit steps", "Add or remove typed steps."],
                ["delete", "Delete", "Remove the saved workflow."]
            ]),
            "",
            Stage::Actions,
        )
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
            .ok_or("No workflow dialog is pending")?;
        if request["op"] != "reply" || request["id"].as_u64() != Some(pending.id) {
            return Err("Unexpected workflow reply".into());
        }
        let value = request.get("value").ok_or("Missing workflow reply")?;
        if !value.is_null() {
            let text = value.as_str().ok_or("Invalid workflow reply")?;
            let valid = match pending.mode {
                "input" => {
                    !text.is_empty() && text.len() <= 4096 && !text.chars().any(char::is_control)
                }
                "choose" => pending.options.iter().any(|o| o == text),
                "output" => text == "close",
                _ => false,
            };
            if !valid {
                return Err("Invalid workflow reply".into());
            }
        }
        let pending = self.pending.take().unwrap();
        if value.is_null() {
            self.route_plans.clear();
            return Ok(Outcome {
                refresh: matches!(
                    pending.stage,
                    Stage::Output | Stage::RouteGrant { .. } | Stage::RouteConfirm { .. }
                ),
                layouts: self.run_layouts.take(),
                ..Outcome::default()
            });
        }
        let text = value.as_str().unwrap();
        match pending.stage {
            Stage::RouteChat(terminal) => {
                let step = Step::RouteTelegram {
                    terminal,
                    chat: text.into(),
                };
                step.validate()?;
                self.steps.push(step);
                self.edit(seq)
            }
            Stage::RouteGrant {
                index,
                target,
                grants,
            } => {
                let (chat, actor) = grants
                    .get(text.parse::<usize>().map_err(|_| "Invalid route grant")?)
                    .copied()
                    .ok_or("Route grant changed")?;
                let super::telegram_admin::Reply::Plan(plan) =
                    source.telegram(super::telegram_admin::Request::Route {
                        chat,
                        actor,
                        target,
                    })?
                else {
                    return Err("Invalid route preview".into());
                };
                if !matches!(plan.as_ref(), super::telegram_admin::Plan::Route(_)) {
                    return Err("Expected an exact Telegram route plan".into());
                }
                self.choose(
                    seq,
                    "Review workflow Telegram route",
                    json!([
                        ["cancel", "Cancel workflow", "Stop remaining workflow steps"],
                        [
                            "apply",
                            "Approve this route",
                            "Exact route and authorization are checked again at apply"
                        ]
                    ]),
                    &super::dialog_text::format(&plan.review())
                        .map_err(|_| "Cannot display route review")?,
                    Stage::RouteConfirm { index, plan },
                )
            }
            Stage::RouteConfirm { index, plan } => {
                if text == "cancel" {
                    self.route_plans.clear();
                    return Ok(Outcome {
                        layouts: self.run_layouts.take(),
                        refresh: true,
                        message: "Remaining workflow steps cancelled".into(),
                        ..Default::default()
                    });
                }
                self.route_plans.insert(index, plan);
                self.execute(source, config, seq)
            }

            Stage::Name => {
                self.name = layouts::validate_name(text)?;
                self.edit(seq)
            }
            Stage::Actions => match text {
                "edit" => self.edit(seq),
                "delete" => self.choose(
                    seq,
                    "Confirm action",
                    json!([
                        ["cancel", "Cancel", "Leave workflow unchanged."],
                        [
                            "yes",
                            "Continue",
                            "Delete this saved workflow; terminals are unaffected."
                        ]
                    ]),
                    &format!("Delete workflow “{}”", self.name),
                    Stage::Delete,
                ),
                _ => self.choose(
                    seq,
                    &format!("Run workflow “{}”", self.name),
                    json!([
                        ["cancel", "Cancel", "Leave everything as it is."],
                        [
                            "run",
                            "Run these steps",
                            "Every terminal is resolved again before its step."
                        ]
                    ]),
                    &self
                        .steps
                        .iter()
                        .enumerate()
                        .map(|(i, s)| format!("{}. {}", i + 1, describe(s)))
                        .collect::<Vec<_>>()
                        .join("\n"),
                    Stage::Run,
                ),
            },
            Stage::Delete => {
                if text == "cancel" {
                    return Ok(Outcome::default());
                }
                let mut layouts = self.view.as_ref().unwrap().layouts.clone();
                layouts
                    .workflows
                    .remove(&self.name)
                    .ok_or("Workflow already removed")?;
                source.save_layouts(&layouts)?;
                Ok(Outcome {
                    layouts: Some(layouts),
                    message: "Workflow deleted".into(),
                    ..Outcome::default()
                })
            }
            Stage::Edit => match text {
                "save" => {
                    let mut layouts = self.view.as_ref().unwrap().layouts.clone();
                    layouts.workflows.insert(
                        self.name.clone(),
                        Workflow {
                            steps: self.steps.clone(),
                            saved_at: String::new(),
                        },
                    );
                    layouts.validate()?;
                    source.save_layouts(&layouts)?;
                    Ok(Outcome {
                        layouts: Some(layouts),
                        message: format!("Workflow “{}” saved privately", self.name),
                        ..Outcome::default()
                    })
                }
                "add" => self.choose(
                    seq,
                    "Add a step",
                    json!([
                        ["open_layout", "Open layout", "No terminal side effects"],
                        ["select", "Select terminal", "No terminal side effects"],
                        ["pin", "Pin terminal", "Private view setting"],
                        [
                            "route_telegram",
                            "Route Telegram",
                            "Choose an authorized chat and review each route when running"
                        ],
                        [
                            "attach",
                            "Open terminal",
                            "Watch or attach to an exact terminal"
                        ]
                    ]),
                    "",
                    Stage::Kind,
                ),
                _ => {
                    let index = text.parse::<usize>().map_err(|_| "Invalid workflow step")?;
                    if index >= self.steps.len() {
                        return Err("Workflow step changed".into());
                    }
                    self.steps.remove(index);
                    self.edit(seq)
                }
            },
            Stage::Kind => {
                if text == "open_layout" {
                    let names = self
                        .view
                        .as_ref()
                        .unwrap()
                        .layouts
                        .layouts
                        .keys()
                        .map(|n| json!([n, n, ""]))
                        .collect::<Vec<_>>();
                    if names.is_empty() {
                        return Err("Save a layout first".into());
                    }
                    return self.choose(seq, "Which layout?", json!(names), "", Stage::Layout);
                }
                let panes = self.view.as_ref().unwrap().snapshot["panes"]
                    .as_array()
                    .cloned()
                    .unwrap_or_default();
                let options = panes
                    .iter()
                    .enumerate()
                    .map(|(i, p)| json!([i.to_string(), tree::title(p, ""), p["pane_id"]]))
                    .collect::<Vec<_>>();
                if options.is_empty() {
                    return Err("No live terminals available".into());
                }
                self.choose(
                    seq,
                    "Which exact terminal?",
                    json!(options),
                    "",
                    Stage::Terminal(text.into(), panes),
                )
            }
            Stage::Layout => {
                self.steps.push(Step::OpenLayout {
                    layout: text.into(),
                });
                self.edit(seq)
            }
            Stage::Terminal(kind, panes) => {
                let pane = panes
                    .get(
                        text.parse::<usize>()
                            .map_err(|_| "Invalid terminal choice")?,
                    )
                    .ok_or("Terminal choice changed")?;
                if kind == "attach" {
                    return self.choose(
                        seq,
                        "Open it how?",
                        json!([
                            ["observe", "Watch (read-only)", "No input is sent."],
                            ["control", "Attach and type", "Keys go to the terminal."]
                        ]),
                        "",
                        Stage::Mode(pane.clone()),
                    );
                }
                let terminal = reference(pane)?;
                if kind == "route_telegram" {
                    return self.frame(
                        seq,
                        "input",
                        "Telegram chat ID",
                        json!({"secret":false}),
                        Stage::RouteChat(terminal),
                    );
                }

                let step = if kind == "pin" {
                    Step::Pin { terminal }
                } else {
                    Step::Select { terminal }
                };
                step.validate()?;
                self.steps.push(step);
                self.edit(seq)
            }
            Stage::Mode(pane) => {
                let step = Step::Attach {
                    terminal: reference(&pane)?,
                    mode: if text == "control" {
                        SavedAttachMode::Control
                    } else {
                        SavedAttachMode::Observe
                    },
                };
                step.validate()?;
                self.steps.push(step);
                self.edit(seq)
            }
            Stage::Run => {
                if text != "run" {
                    return Ok(Outcome::default());
                }
                self.route_plans.clear();
                self.run_index = 0;
                self.run_results.clear();
                self.run_layouts = None;
                self.execute(source, config, seq)
            }
            Stage::Output => Ok(Outcome {
                refresh: true,
                ..Outcome::default()
            }),
        }
    }
    fn prepare_routes<S: Source>(
        &mut self,
        start: usize,
        source: &mut S,
        config: &ControlConfig,
        seq: &mut u64,
    ) -> Result<Outcome> {
        let next = self
            .steps
            .iter()
            .enumerate()
            .skip(start)
            .find_map(|(index, step)| {
                if let Step::RouteTelegram { terminal, chat } = step {
                    Some((index, terminal.clone(), chat.clone()))
                } else {
                    None
                }
            });
        let Some((index, terminal, chat)) = next else {
            return self.execute(source, config, seq);
        };
        let chat = chat.parse::<i64>().map_err(|_| "Invalid workflow chat")?;
        let observed = source.refresh(config)?;
        let candidates = observed.snapshot["panes"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|p| reference(p).is_ok_and(|r| r.same_identity(&terminal)))
            .collect::<Vec<_>>();
        if candidates.len() != 1 {
            return Err("Workflow route terminal is stale or ambiguous".into());
        }
        let identity = crate::adapters::terminals::identity(candidates[0])?;
        let super::telegram_admin::Reply::Data(data) =
            source.telegram(super::telegram_admin::Request::Routes)?
        else {
            return Err("Invalid route inventory".into());
        };
        let terminals: Vec<crate::core::routes::Terminal> =
            serde_json::from_value(data["terminals"].clone())
                .map_err(|_| "Invalid route terminals")?;
        let targets = terminals
            .into_iter()
            .filter(|t| t.available && t.identity == identity)
            .collect::<Vec<_>>();
        if targets.len() != 1 {
            return Err("Workflow route terminal changed".into());
        }
        let grants = data["grants"]
            .as_array()
            .ok_or("Invalid route grants")?
            .iter()
            .filter_map(|g| Some((g["chat"].as_i64()?, g["actor"].as_i64()?)))
            .filter(|(candidate, _)| *candidate == chat)
            .collect::<Vec<_>>();
        if grants.is_empty() {
            return Err("Workflow chat has no configured route manager".into());
        }
        let choices = grants
            .iter()
            .enumerate()
            .map(|(i, (chat, actor))| {
                json!([
                    i.to_string(),
                    format!("Chat {chat} · authorized user {actor}"),
                    "Use this explicit configured grant"
                ])
            })
            .collect::<Vec<_>>();
        self.choose(
            seq,
            "Authorize workflow Telegram route",
            json!(choices),
            "Choose the configured route manager; no chat is inferred.",
            Stage::RouteGrant {
                index,
                target: Box::new(targets.into_iter().next().unwrap()),
                grants,
            },
        )
    }
    fn execute<S: Source>(
        &mut self,
        source: &mut S,
        config: &ControlConfig,
        seq: &mut u64,
    ) -> Result<Outcome> {
        let mut frames = Vec::new();
        let mut results = std::mem::take(&mut self.run_results);
        let mut layouts = self
            .run_layouts
            .take()
            .unwrap_or_else(|| self.view.as_ref().unwrap().layouts.clone());
        for (index, step) in self.steps.clone().iter().enumerate().skip(self.run_index) {
            if matches!(step, Step::RouteTelegram { .. }) && !self.route_plans.contains_key(&index)
            {
                self.run_index = index;
                self.run_results = results;
                self.run_layouts = Some(layouts);
                let mut outcome = self.prepare_routes(index, source, config, seq)?;
                frames.append(&mut outcome.frames);
                outcome.frames = frames;
                return Ok(outcome);
            }
            let result = (|| -> Result<String> {
                step.validate()?;
                let observed = source.refresh(config)?;
                let pane = match step {
                    Step::Select { terminal }
                    | Step::Pin { terminal }
                    | Step::Attach { terminal, .. }
                    | Step::RouteTelegram { terminal, .. } => {
                        let candidates = observed.snapshot["panes"]
                            .as_array()
                            .into_iter()
                            .flatten()
                            .filter(|p| reference(p).is_ok_and(|r| r.same_identity(terminal)))
                            .collect::<Vec<_>>();
                        if candidates.len() != 1 {
                            return Err(
                                "Missing, stale or ambiguous terminal; workflow stopped".into()
                            );
                        }
                        Some(candidates[0].clone())
                    }
                    _ => None,
                };
                match step {
                    Step::OpenLayout { layout } => {
                        layouts.open_layout(layout)?;
                        source.save_layouts(&layouts)?;
                        Ok(format!("Opened {layout}"))
                    }
                    Step::Select { terminal } | Step::Pin { terminal } => {
                        layouts.current.selected = Some(terminal.clone());
                        if matches!(step, Step::Pin { .. })
                            && !layouts
                                .current
                                .pins
                                .iter()
                                .any(|p| p.same_identity(terminal))
                        {
                            layouts.toggle_pin(terminal.clone())?;
                        }
                        source.save_layouts(&layouts)?;
                        Ok(describe(step))
                    }
                    Step::Attach { mode, .. } => {
                        let pane = pane.unwrap();
                        let node = tree::Tree::build(
                            &observed.snapshot,
                            &observed.health,
                            &tree::ProjectionContext::default(),
                        )
                        .nodes
                        .into_iter()
                        .find(|node| node.pane.as_ref() == Some(&pane))
                        .map(|node| node.id())
                        .unwrap_or_default();
                        let Answer::Frame(plan) = source.session(
                            config,
                            Request::Attach {
                                pane,
                                mode: if matches!(mode, SavedAttachMode::Control) {
                                    AttachMode::Control
                                } else {
                                    AttachMode::Observe
                                },
                                node,
                            },
                        )?
                        else {
                            return Err("Invalid attach response".into());
                        };
                        frames.push(json!({"version":2,"kind":"attach","plan":plan}));
                        Ok(describe(step))
                    }
                    Step::RouteTelegram { .. } => {
                        let plan = self
                            .route_plans
                            .remove(&index)
                            .ok_or("Workflow route was not reviewed")?;
                        let approval = plan.approval();
                        let super::telegram_admin::Reply::Data(_) = source
                            .telegram(super::telegram_admin::Request::Apply { plan, approval })?
                        else {
                            return Err("Invalid route apply response".into());
                        };
                        Ok("Reviewed Telegram route applied".into())
                    }
                }
            })();
            match result {
                Ok(message) => results.push(format!("{}. {message}", index + 1)),
                Err(error) => {
                    results.push(format!("{}. Stopped: {error}", index + 1));
                    break;
                }
            }
        }
        self.run_index = 0;
        self.run_layouts = None;
        self.run_results.clear();
        self.route_plans.clear();
        let text = if results.is_empty() {
            "No steps ran".into()
        } else {
            results.join("\n")
        };
        let mut outcome = self.frame(
            seq,
            "output",
            &format!("Workflow “{}”", self.name),
            json!({"text":text}),
            Stage::Output,
        )?;
        frames.append(&mut outcome.frames);
        outcome.frames = frames;
        outcome.layouts = Some(layouts);
        outcome.restore = true;
        Ok(outcome)
    }
}
fn describe(step: &Step) -> String {
    match step {
        Step::OpenLayout { layout } => format!("Open layout {layout}"),
        Step::Select { terminal } => format!("Select {}", terminal.label),
        Step::Pin { terminal } => format!("Pin {}", terminal.label),
        Step::RouteTelegram { .. } => "Route Telegram (requires grant selection and review)".into(),
        Step::Attach { terminal, mode } => format!(
            "{} {}",
            if matches!(mode, SavedAttachMode::Control) {
                "Attach"
            } else {
                "Watch"
            },
            terminal.label
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::super::backend::Observations;
    use super::*;
    use std::path::Path;
    #[derive(Default)]
    struct Mock {
        snapshots: Vec<Value>,
        reads: usize,
        saves: Vec<layouts::Layouts>,
        attaches: usize,
        fail: bool,
        route_directory: Option<std::path::PathBuf>,
        route_applies: usize,
    }
    impl Source for Mock {
        fn telegram(
            &mut self,
            request: super::super::telegram_admin::Request,
        ) -> Result<super::super::telegram_admin::Reply> {
            use super::super::telegram_admin::{Plan, Reply, Request, RoutePlan};
            let terminal = crate::core::routes::Terminal {
                identity: crate::adapters::terminals::identity(&pane()).unwrap(),
                label: "fixture".into(),
                workspace: "fixture".into(),
                harness: "claude".into(),
                conversation: Some("session".into()),
                available: true,
            };
            match request {
                Request::Routes => Ok(Reply::Data(
                    json!({"grants":[{"chat":42,"actor":7}],"terminals":[terminal]}),
                )),
                Request::Route {
                    chat,
                    actor,
                    target,
                } => Ok(Reply::Plan(Box::new(Plan::Route(Box::new(
                    RoutePlan::prepare(
                        self.route_directory.as_ref().unwrap(),
                        chat,
                        actor,
                        *target,
                        &[terminal],
                    )?,
                ))))),
                Request::Apply { plan, approval } => {
                    assert_eq!(approval, plan.approval());
                    assert!(matches!(plan.as_ref(), Plan::Route(_)));
                    self.route_applies += 1;
                    if self.fail {
                        Err("Route changed before apply".into())
                    } else {
                        Ok(Reply::Data(json!({"applied":true})))
                    }
                }
                _ => panic!("Unexpected Telegram operation"),
            }
        }

        fn refresh(&mut self, _: &ControlConfig) -> Result<Observations> {
            let snapshot = self
                .snapshots
                .get(self.reads)
                .cloned()
                .unwrap_or_else(|| json!({"panes":[]}));
            self.reads += 1;
            Ok(Observations {
                snapshot,
                ..Default::default()
            })
        }
        fn preview(&mut self, _: &Value, _: usize) -> Result<String> {
            unreachable!()
        }
        fn save_layouts(&mut self, value: &layouts::Layouts) -> Result<()> {
            self.saves.push(value.clone());
            Ok(())
        }
        fn session(&mut self, _: &ControlConfig, request: Request) -> Result<Answer> {
            assert!(matches!(request, Request::Attach { .. }));
            self.attaches += 1;
            if self.fail {
                Err("identity changed at apply".into())
            } else {
                Ok(Answer::Frame(json!({"mode":"observe","argv":["mock"]})))
            }
        }
    }
    fn pane() -> Value {
        json!({"backend":"tmux","server":"tmux@/tmp/mock.sock","socket":"/tmp/mock.sock","pane_id":"%1","terminal_id":"original","session":"project"})
    }
    fn view(steps: Vec<Step>) -> View {
        let mut layouts = layouts::Layouts::default();
        layouts.workflows.insert(
            "test".into(),
            Workflow {
                steps,
                saved_at: String::new(),
            },
        );
        View {
            snapshot: json!({"panes":[pane()]}),
            health: json!({}),
            layouts,
            selected: None,
            checkpoint: None,
            node: String::new(),
        }
    }
    fn reply(menu: &mut Menu, mock: &mut Mock, seq: &mut u64, value: Value) -> Outcome {
        menu.reply(
            &json!({"op":"reply","id":*seq,"value":value}),
            mock,
            &ControlConfig::fresh(Path::new("/tmp/project")).unwrap(),
            seq,
        )
        .unwrap()
    }
    #[test]
    fn edit_delete_cancel_and_confirmation_replay() {
        let (mut menu, mut mock, mut seq) = (Menu::default(), Mock::default(), 0);
        menu.start("workflow.new", view(vec![]), &mut seq).unwrap();
        reply(&mut menu, &mut mock, &mut seq, json!("new"));
        reply(&mut menu, &mut mock, &mut seq, json!("save"));
        assert!(mock.saves[0].workflows.contains_key("new"));
        menu.start("workflow:test", view(vec![]), &mut seq).unwrap();
        reply(&mut menu, &mut mock, &mut seq, json!("delete"));
        reply(&mut menu, &mut mock, &mut seq, json!("cancel"));
        assert_eq!(mock.saves.len(), 1);
        menu.start("workflow:test", view(vec![]), &mut seq).unwrap();
        reply(&mut menu, &mut mock, &mut seq, json!("delete"));
        let id = seq;
        reply(&mut menu, &mut mock, &mut seq, json!("yes"));
        assert!(!mock.saves[1].workflows.contains_key("test"));
        assert!(
            menu.reply(
                &json!({"op":"reply","id":id,"value":"yes"}),
                &mut mock,
                &ControlConfig::fresh(Path::new("/tmp/project")).unwrap(),
                &mut seq
            )
            .is_err()
        );
        assert_eq!(mock.saves.len(), 2);
    }
    #[test]
    fn runner_refreshes_each_step_and_stops_before_stale_pin() {
        let terminal = reference(&pane()).unwrap();
        let steps = vec![
            Step::Attach {
                terminal: terminal.clone(),
                mode: SavedAttachMode::Observe,
            },
            Step::Pin { terminal },
        ];
        let mut stale = pane();
        stale["terminal_id"] = json!("replacement");
        let mut mock = Mock {
            snapshots: vec![json!({"panes":[pane()]}), json!({"panes":[stale]})],
            ..Default::default()
        };
        let (mut menu, mut seq) = (Menu::default(), 0);
        menu.start("workflow:test", view(steps), &mut seq).unwrap();
        reply(&mut menu, &mut mock, &mut seq, json!("run"));
        assert_eq!(mock.reads, 0);
        let out = reply(&mut menu, &mut mock, &mut seq, json!("run"));
        assert_eq!(mock.reads, 2);
        assert_eq!(mock.attaches, 1);
        assert!(mock.saves.is_empty());
        assert_eq!(out.frames[0]["kind"], "attach");
        assert!(out.frames[1]["text"].as_str().unwrap().contains("Stopped"));
    }
    #[test]
    fn failed_apply_stops_following_steps_and_cancel_has_no_effect() {
        let terminal = reference(&pane()).unwrap();
        let steps = vec![
            Step::Attach {
                terminal: terminal.clone(),
                mode: SavedAttachMode::Control,
            },
            Step::Pin { terminal },
        ];
        let mut mock = Mock {
            snapshots: vec![json!({"panes":[pane()]})],
            fail: true,
            ..Default::default()
        };
        let (mut menu, mut seq) = (Menu::default(), 0);
        menu.start("workflow:test", view(steps.clone()), &mut seq)
            .unwrap();
        reply(&mut menu, &mut mock, &mut seq, json!("run"));
        reply(&mut menu, &mut mock, &mut seq, Value::Null);
        assert_eq!(mock.reads, 0);
        menu.start("workflow:test", view(steps), &mut seq).unwrap();
        reply(&mut menu, &mut mock, &mut seq, json!("run"));
        let out = reply(&mut menu, &mut mock, &mut seq, json!("run"));
        assert_eq!(mock.reads, 1);
        assert!(mock.saves.is_empty());
        assert!(
            out.frames[0]["text"]
                .as_str()
                .unwrap()
                .contains("identity changed at apply")
        );
    }
    #[test]
    fn workflow_route_requires_explicit_grant_review_and_resumes_remaining_steps() {
        use std::{fs, os::unix::fs::PermissionsExt};
        let directory =
            std::env::temp_dir().join(format!("daedalus-workflow-route-{}", std::process::id()));
        fs::create_dir(&directory).unwrap();
        fs::set_permissions(&directory, fs::Permissions::from_mode(0o700)).unwrap();
        let config = json!({"version":1,"instance":"fixture","bot_id":123,"token_file":directory.join("token"),"grants":[{"chat":42,"actor":7,"permissions":["read","send","manage"]}],"listen_chats":[],"registrations":[],"hook_bindings":[]});
        layouts::write_private(
            &directory.join("telegram-service.json"),
            &serde_json::to_vec(&config).unwrap(),
        )
        .unwrap();
        let terminal = reference(&pane()).unwrap();
        let steps = vec![
            Step::Pin {
                terminal: terminal.clone(),
            },
            Step::RouteTelegram {
                terminal: terminal.clone(),
                chat: "42".into(),
            },
            Step::Select { terminal },
        ];
        let mut mock = Mock {
            snapshots: vec![json!({"panes":[pane()]}); 8],
            route_directory: Some(directory.clone()),
            ..Default::default()
        };
        let (mut menu, mut seq) = (Menu::default(), 0);
        menu.start("workflow:test", view(steps), &mut seq).unwrap();
        reply(&mut menu, &mut mock, &mut seq, json!("run"));
        let grant = reply(&mut menu, &mut mock, &mut seq, json!("run"));
        assert_eq!(
            grant.frames[0]["title"],
            "Authorize workflow Telegram route"
        );
        assert_eq!(mock.saves.len(), 1);
        assert_eq!(mock.route_applies, 0);
        let review = reply(&mut menu, &mut mock, &mut seq, json!("0"));
        assert_eq!(review.frames[0]["title"], "Review workflow Telegram route");
        assert_eq!(mock.route_applies, 0);
        assert!(!directory.join("routes.sqlite3").exists());
        let done = reply(&mut menu, &mut mock, &mut seq, json!("apply"));
        assert_eq!(mock.route_applies, 1);
        assert_eq!(mock.saves.len(), 2);
        assert!(
            done.frames[0]["text"]
                .as_str()
                .unwrap()
                .contains("Reviewed Telegram route applied")
        );
        fs::remove_dir_all(directory).unwrap();
    }
}
