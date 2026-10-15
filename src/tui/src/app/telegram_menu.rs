//! Telegram dialogs using the existing renderer protocol.
use super::{
    backend::Source,
    config::ControlConfig,
    session_menu::Outcome,
    telegram_admin::{Plan, Reply, Request},
    telegram_config::{Access, Grant, ServiceConfig},
};
use serde_json::{Value, json};
use std::{collections::BTreeSet, path::PathBuf};
type Result<T> = std::result::Result<T, String>;
struct Details {
    field: usize,
    expected: Option<ServiceConfig>,
    updated: ServiceConfig,
    actor: Option<i64>,
}
enum Stage {
    Output,
    Details(Box<Details>),
    Token,
    RouteGrant(Value),
    RouteTarget {
        chat: i64,
        actor: i64,
        terminals: Vec<crate::core::routes::Terminal>,
    },
    RouteBindings {
        chat: i64,
        actor: i64,
        choices: Vec<Value>,
    },
    Confirm(Box<Plan>),
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
}
pub fn available(action: &str) -> bool {
    matches!(
        action,
        "telegram.details"
            | "telegram.token"
            | "telegram.start"
            | "telegram.stop"
            | "telegram.route"
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
        let choices = frame["options"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|v| v[0].as_str().map(str::to_owned))
            .collect();
        self.pending = Some(Pending {
            choices,
            id: *seq,
            mode,
            stage,
        });
        Ok(Outcome {
            frames: vec![frame],
            ..Default::default()
        })
    }
    fn review(&mut self, seq: &mut u64, reply: Reply) -> Result<Outcome> {
        let Reply::Plan(plan) = reply else {
            return Err("Expected reviewed Telegram plan".into());
        };
        self.frame(seq,"choose","Review Telegram changes",json!({"description":super::dialog_text::format(&plan.review()).map_err(|_|"Cannot display Telegram review")?,"options":[["apply","Apply reviewed changes","Apply this plan once"],["cancel","Cancel","Leave configuration unchanged"]]}),Stage::Confirm(plan))
    }
    pub fn start<S: Source>(
        &mut self,
        action: &str,
        source: &mut S,
        _config: &ControlConfig,
        seq: &mut u64,
    ) -> Result<Outcome> {
        if self.pending() {
            return Err("Finish the current Telegram dialog first".into());
        }
        match action {
            "telegram.route" => {
                let Reply::Data(data)=source.telegram(Request::Routes)? else {return Err("Invalid route inventory".into());};
                let choices=data["grants"].as_array().ok_or("Invalid route grants")?.iter().enumerate().map(|(i,g)|json!([i.to_string(),format!("Chat {}",g["chat"]),format!("Authorized user {}",g["actor"])] )).collect::<Vec<_>>();
                if choices.is_empty(){return Err("Configure a Telegram chat and route manager first".into());}
                self.frame(seq,"choose","Choose Telegram chat",json!({"options":choices}),Stage::RouteGrant(data))
            }
            "telegram.token" => self.frame(seq,"input","Telegram bot token",json!({"secret":true,"description":"Paste the token for the configured bot. It is stored privately and never shown in the review."}),Stage::Token),
            "telegram.start"|"telegram.stop" => self.review(seq,source.telegram(Request::Service{start:action=="telegram.start"})?),
            "telegram.details" => {
                let Reply::Data(value)=source.telegram(Request::Inspect)? else {return Err("Invalid Telegram settings response".into());};
                let expected:Option<ServiceConfig>=value.get("settings").map(|v|serde_json::from_value(v.clone()).map_err(|_|"Invalid Telegram settings")).transpose()?;
                let directory=PathBuf::from(value["config_directory"].as_str().ok_or("Missing configuration directory")?);
                let updated=expected.clone().unwrap_or(ServiceConfig {version:1,instance:String::new(),bot_id:0,token_file:directory.join("telegram.env"),grants:vec![],listen_chats:BTreeSet::new(),registrations:vec![],hook_bindings:vec![]});
                let field=if expected.is_some(){2}else{0};
                self.field(seq,field,expected,updated,None)
            }
            _=>Err("Unknown Telegram action".into()),
        }
    }
    fn field(
        &mut self,
        seq: &mut u64,
        field: usize,
        expected: Option<ServiceConfig>,
        updated: ServiceConfig,
        actor: Option<i64>,
    ) -> Result<Outcome> {
        let title = match field {
            0 => "Instance name",
            1 => "Telegram bot ID",
            2 => "Telegram user ID to authorize",
            _ => "Telegram chat ID",
        };
        self.frame(seq,"input",title,json!({"secret":false,"description":if field>=2 {"This user and chat receive explicit read, send, and route-management permission. Existing grants are preserved."}else{"Use the bot's numeric ID and a portable instance name. The token is configured separately."}}),Stage::Details(Box::new(Details{field,expected,updated,actor})))
    }
    pub fn answer<S: Source>(
        &mut self,
        source: &mut S,
        _config: &ControlConfig,
        request: &Value,
        seq: &mut u64,
    ) -> Result<Outcome> {
        let pending = self.pending.as_ref().ok_or("No Telegram dialog pending")?;
        if request["op"] != "reply" || request["id"].as_u64() != Some(pending.id) {
            return Err("Unexpected Telegram reply".into());
        }
        let value = request.get("value").ok_or("Missing Telegram reply")?;
        if !value.is_null() {
            let text = value.as_str().ok_or("Invalid Telegram reply")?;
            if (pending.mode == "output" && text != "close")
                || (pending.mode == "choose" && !pending.choices.iter().any(|v| v == text))
                || (pending.mode == "input"
                    && (text.is_empty() || text.len() > 1024 || text.chars().any(char::is_control)))
            {
                return Err("Invalid Telegram reply".into());
            }
        }
        let pending = self.pending.take().unwrap();
        if value.is_null() {
            return Ok(Outcome::default());
        }
        let text = value.as_str().unwrap();
        match pending.stage {
            Stage::Output => Ok(Outcome::default()),
            Stage::RouteGrant(data) => {
                let index = text
                    .parse::<usize>()
                    .map_err(|_| "Invalid chat selection")?;
                let grant = &data["grants"][index];
                let chat = grant["chat"].as_i64().ok_or("Invalid chat")?;
                let actor = grant["actor"].as_i64().ok_or("Invalid actor")?;
                let terminals: Vec<crate::core::routes::Terminal> =
                    serde_json::from_value(data["terminals"].clone())
                        .map_err(|_| "Invalid terminal inventory")?;
                let choices = terminals
                    .iter()
                    .enumerate()
                    .filter(|(_, t)| t.available)
                    .map(|(i, t)| {
                        json!([
                            i.to_string(),
                            t.label,
                            format!("{} · {}", t.harness, t.workspace)
                        ])
                    })
                    .collect::<Vec<_>>();
                if choices.is_empty() {
                    return Err("No live terminal is available".into());
                }
                self.frame(
                    seq,
                    "choose",
                    "Choose destination terminal",
                    json!({"options":choices}),
                    Stage::RouteTarget {
                        chat,
                        actor,
                        terminals,
                    },
                )
            }
            Stage::RouteTarget {
                chat,
                actor,
                terminals,
            } => {
                let index = text
                    .parse::<usize>()
                    .map_err(|_| "Invalid terminal selection")?;
                let target = terminals
                    .get(index)
                    .ok_or("Invalid terminal selection")?
                    .clone();
                self.review(
                    seq,
                    source.telegram(Request::Route {
                        chat,
                        actor,
                        target: Box::new(target),
                    })?,
                )
            }
            Stage::RouteBindings {
                chat,
                actor,
                choices,
            } => {
                if text == "done" {
                    return Ok(Outcome::default());
                }
                let selected = choices
                    .get(
                        text.parse::<usize>()
                            .map_err(|_| "Invalid hook selection")?,
                    )
                    .ok_or("Hook selection changed")?;
                let registration = PathBuf::from(
                    selected["registration_path"]
                        .as_str()
                        .ok_or("Missing hook path")?,
                );
                self.review(
                    seq,
                    source.telegram(Request::Bind {
                        registration,
                        chat,
                        actor,
                    })?,
                )
            }
            Stage::Token => {
                self.review(seq, source.telegram(Request::Token { value: text.into() })?)
            }
            Stage::Confirm(plan) => {
                if text == "cancel" {
                    return Ok(Outcome::default());
                }
                let route = if matches!(*plan, Plan::Route(_)) {
                    let review = plan.review();
                    Some((
                        review["chat"]
                            .as_str()
                            .ok_or("Missing route chat")?
                            .parse::<i64>()
                            .map_err(|_| "Invalid chat")?,
                        review["actor"]
                            .as_str()
                            .ok_or("Missing route actor")?
                            .parse::<i64>()
                            .map_err(|_| "Invalid actor")?,
                    ))
                } else {
                    None
                };
                let approval = plan.approval();
                let Reply::Data(result) = source.telegram(Request::Apply { plan, approval })?
                else {
                    return Err("Invalid Telegram result".into());
                };
                if let Some((chat, actor)) = route {
                    let Reply::Data(data) =
                        source.telegram(Request::RouteBindings { chat, actor })?
                    else {
                        return Err("Invalid hook inventory".into());
                    };
                    let choices = data["recipients"]
                        .as_array()
                        .cloned()
                        .ok_or("Invalid hook inventory")?;
                    if !choices.is_empty() {
                        let mut options = vec![json!([
                            "done",
                            "Done",
                            "Keep the selected route without adding hook recipients"
                        ])];
                        options.extend(choices.iter().enumerate().map(|(i, v)| {
                            json!([
                                i.to_string(),
                                format!("Connect {}", v["registration"].as_str().unwrap_or("hook")),
                                format!(
                                    "Conversation {}",
                                    v["conversation"].as_str().unwrap_or("")
                                )
                            ])
                        }));
                        let mut outcome = self.frame(seq,"choose","Route saved · connect replies",json!({"options":options,"description":"Select one exact installed hook registration. Recipient permission is reviewed separately."}),Stage::RouteBindings { chat, actor, choices })?;
                        outcome.refresh = true;
                        return Ok(outcome);
                    }
                }
                let mut outcome = self.frame(
                    seq,
                    "output",
                    "Telegram result",
                    json!({"text": super::dialog_text::review(&result)}),
                    Stage::Output,
                )?;
                outcome.refresh = true;
                Ok(outcome)
            }
            Stage::Details(details) => {
                let Details {
                    field,
                    expected,
                    mut updated,
                    mut actor,
                } = *details;
                match field {
                    0 => updated.instance = text.into(),
                    1 => {
                        updated.bot_id = text
                            .parse::<i64>()
                            .ok()
                            .filter(|v| *v > 0)
                            .ok_or("Bot ID must be positive")?
                    }
                    2 => {
                        actor = Some(
                            text.parse::<i64>()
                                .ok()
                                .filter(|v| *v > 0)
                                .ok_or("User ID must be positive")?,
                        )
                    }
                    _ => {
                        let chat = text
                            .parse::<i64>()
                            .ok()
                            .filter(|v| *v != 0)
                            .ok_or("Chat ID must be a nonzero integer")?;
                        let actor = actor.ok_or("Missing authorized user")?;
                        updated
                            .grants
                            .retain(|g| g.actor != actor || g.chat != chat);
                        updated.grants.push(Grant {
                            chat,
                            actor,
                            permissions: vec![Access::Read, Access::Send, Access::Manage],
                        });
                        return self.review(
                            seq,
                            source.telegram(Request::Configure {
                                expected: expected.map(Box::new),
                                config: Box::new(updated),
                            })?,
                        );
                    }
                }
                self.field(seq, field + 1, expected, updated, actor)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        app::backend::Observations,
        core::layouts::{self, Layouts},
    };
    struct Local {
        directory: PathBuf,
        applied: usize,
    }
    impl Local {
        fn new() -> Self {
            let directory = std::env::temp_dir().join(format!(
                "daedalus-telegram-menu-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
            layouts::write_private(&directory.join("fixture"), b"fixture").unwrap();
            Self {
                directory,
                applied: 0,
            }
        }
    }
    impl Drop for Local {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.directory);
        }
    }
    impl Source for Local {
        fn telegram(&mut self, request: Request) -> Result<Reply> {
            if matches!(&request, Request::Apply { .. }) {
                self.applied += 1;
            }
            super::super::telegram_admin::execute(&self.directory, request)
        }
        fn refresh(&mut self, _: &ControlConfig) -> Result<Observations> {
            unreachable!()
        }
        fn preview(&mut self, _: &Value, _: usize) -> Result<String> {
            unreachable!()
        }
        fn save_layouts(&mut self, _: &Layouts) -> Result<()> {
            unreachable!()
        }
    }
    fn config() -> ControlConfig {
        ControlConfig::fresh(std::path::Path::new(crate::test_support::path(
            "/tmp/project",
        )))
        .unwrap()
    }
    fn answer(menu: &mut Menu, local: &mut Local, seq: &mut u64, value: Value) -> Outcome {
        menu.answer(
            local,
            &config(),
            &json!({"op":"reply","id":*seq,"value":value}),
            seq,
        )
        .unwrap()
    }
    fn details(menu: &mut Menu, local: &mut Local, seq: &mut u64) {
        menu.start("telegram.details", local, &config(), seq)
            .unwrap();
        for text in ["fixture", "123", "456", "-100789"] {
            answer(menu, local, seq, json!(text));
        }
    }
    #[test]
    fn fresh_details_require_review_and_confirmation_is_single_use() {
        let (mut menu, mut local, mut seq) = (Menu::default(), Local::new(), 0);
        details(&mut menu, &mut local, &mut seq);
        assert!(!local.directory.join("telegram-service.json").exists());
        let id = seq;
        answer(&mut menu, &mut local, &mut seq, json!("apply"));
        let settings = ServiceConfig::load(&local.directory.join("telegram-service.json")).unwrap();
        assert_eq!(settings.grants[0].actor, 456);
        assert_eq!(settings.grants[0].chat, -100789);
        assert!(!settings.token_file.exists());
        assert!(
            menu.answer(
                &mut local,
                &config(),
                &json!({"op":"reply","id":id,"value":"apply"}),
                &mut seq
            )
            .is_err()
        );
        assert_eq!(local.applied, 1);
    }
    #[test]
    fn token_is_masked_review_is_redacted_and_cancel_is_inert() {
        let (mut menu, mut local, mut seq) = (Menu::default(), Local::new(), 0);
        details(&mut menu, &mut local, &mut seq);
        let result = answer(&mut menu, &mut local, &mut seq, json!("apply"));
        assert_eq!(result.frames[0]["mode"], "output");
        assert!(!result.frames[0]["text"].as_str().unwrap().starts_with('{'));
        answer(&mut menu, &mut local, &mut seq, json!("close"));
        let out = menu
            .start("telegram.token", &mut local, &config(), &mut seq)
            .unwrap();
        assert_eq!(out.frames[0]["secret"], true);
        let out = answer(&mut menu, &mut local, &mut seq, json!("123:fixture_secret"));
        assert!(!format!("{out:?}").contains("fixture_secret"));
        answer(&mut menu, &mut local, &mut seq, json!("cancel"));
        assert!(!local.directory.join("telegram.env").exists());
        assert_eq!(local.applied, 1);
    }
}
