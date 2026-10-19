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
#[derive(Clone)]
struct Details {
    field: usize,
    expected: Option<ServiceConfig>,
    updated: ServiceConfig,
    actor: Option<i64>,
}
#[derive(Clone, Copy)]
enum Next {
    Token,
    Start,
    Route,
}
impl Next {
    fn action(self) -> &'static str {
        match self {
            Self::Token => "telegram.token",
            Self::Start => "telegram.start",
            Self::Route => "telegram.route",
        }
    }
    fn label(self) -> &'static str {
        match self {
            Self::Token => "Save the bot token",
            Self::Start => "Review starting Telegram",
            Self::Route => "Choose a destination terminal",
        }
    }
    fn help(self) -> &'static str {
        match self {
            Self::Token => {
                "Get your bot token from @BotFather in Telegram. It is stored in your private configuration directory."
            }
            Self::Start => {
                "Starting connects to Telegram. It requires a separate review and confirmation; saving settings alone does not start it."
            }
            Self::Route => {
                "Select the terminal that should receive messages, then connect an installed reply hook. Start a chat with your bot before sending a message."
            }
        }
    }
}
enum Stage {
    Output(Option<Next>),
    Continue(Next),
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
        if action == "telegram.start" {
            let Reply::Data(status) = source.telegram(Request::Inspect)? else {
                return Err("Invalid Telegram settings response".into());
            };
            if status["configured"] == false {
                return self.start("telegram.details", source, _config, seq);
            }
            if status["token_locally_valid"] != true {
                return self.start("telegram.token", source, _config, seq);
            }
        }
        match action {
            "telegram.route" => {
                let Reply::Data(data)=source.telegram(Request::Routes)? else {return Err("Invalid route inventory".into());};
                let choices=data["grants"].as_array().ok_or("Invalid route grants")?.iter().enumerate().map(|(i,g)|json!([i.to_string(),format!("Chat {}",g["chat"]),format!("Authorized user {}",g["actor"])] )).collect::<Vec<_>>();
                if choices.is_empty(){return Err("No authorized chat yet. Open Telegram connection settings and add your user ID and chat ID, then choose a destination.".into());}
                self.frame(seq,"choose","Choose Telegram chat",json!({"options":choices}),Stage::RouteGrant(data))
            }
            "telegram.token" => self.frame(seq,"input","Telegram bot token",json!({"secret":true,"description":"Paste the complete token from @BotFather (numeric ID:secret). It must match your configured bot ID. It is stored privately and never shown in the review."}),Stage::Token),
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
        let description = match field {
            0 => {
                "Choose a local connection name: 1–64 letters, numbers, underscores or hyphens. This is not your Telegram username."
            }
            1 => {
                "Enter the numeric part before the colon in the token provided by @BotFather. The full token is saved privately in the next step."
            }
            2 => {
                "Enter your numeric Telegram user ID, not your @username. Only this user will receive read, send and route-management permission for the chosen chat. Existing permissions are preserved."
            }
            _ => {
                "For a private conversation, use the same numeric ID as your user ID. For a group, use its chat ID, including the leading minus sign. Existing chat permissions are preserved."
            }
        };
        self.frame(
            seq,
            "input",
            title,
            json!({"secret":false,"description":description}),
            Stage::Details(Box::new(Details {
                field,
                expected,
                updated,
                actor,
            })),
        )
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
        // Validate before consuming the dialog so a typo does not discard earlier fields.
        if let (Stage::Details(details), Some(text)) = (&pending.stage, value.as_str()) {
            let error = match details.field {
                0 if text.len() > 64
                    || !text
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b"_-".contains(&b)) =>
                {
                    Some("Use 1–64 letters, numbers, underscores or hyphens.")
                }
                1 | 2 if text.parse::<i64>().ok().filter(|id| *id > 0).is_none() => {
                    Some("Enter a positive numeric ID, without @ or spaces.")
                }
                3 if text.parse::<i64>().ok().filter(|id| *id != 0).is_none() => {
                    Some("Enter a nonzero numeric chat ID. Keep the minus sign for a group.")
                }
                _ => None,
            };
            if let Some(error) = error {
                let details = (**details).clone();
                let mut outcome = self.field(
                    seq,
                    details.field,
                    details.expected,
                    details.updated,
                    details.actor,
                )?;
                let frame = &mut outcome.frames[0];
                frame["description"] = json!(format!(
                    "{error}\n\n{}",
                    frame["description"].as_str().unwrap_or("")
                ));
                return Ok(outcome);
            }
        }
        let pending = self.pending.take().unwrap();
        if value.is_null() {
            return Ok(Outcome::default());
        }
        let text = value.as_str().unwrap();
        match pending.stage {
            Stage::Output(None) => Ok(Outcome::default()),
            Stage::Output(Some(next)) => self.frame(
                seq, "choose", "Telegram · next step",
                json!({"description":next.help(),"options":[["continue",next.label(),"Review the next step before applying it."],["later","Finish later","Keep saved changes; return from the Telegram menu."]]}),
                Stage::Continue(next),
            ),
            Stage::Continue(next) => {
                if text == "later" { return Ok(Outcome::default()); }
                self.start(next.action(), source, _config, seq)
            }
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
                    return Err("No controllable terminal is available. Open an agent terminal in Daedalus, refresh the terminal list, then choose a destination again.".into());
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
                match source.telegram(Request::Token { value: text.into() }) {
                    Ok(reply) => self.review(seq, reply),
                    Err(_) => self.frame(
                        seq, "input", "Check Telegram bot token",
                        json!({"secret":true,"description":"The token could not be prepared. Paste the complete token from @BotFather and check that its numeric prefix matches your configured bot ID. If it matches, check that your private configuration directory is writable. No token was saved."}),
                        Stage::Token,
                    ),
                }
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
                let (summary, mut next) = match &*plan {
                    Plan::Configuration(_) => ("Telegram settings saved. Saving settings does not start or reconnect the bot.", Some(Next::Token)),
                    Plan::Token(_) => ("Bot token saved privately. Its format matches the configured bot; Telegram has not verified it yet.", Some(Next::Start)),
                    Plan::Service(_) if plan.review()["operation"] == "start" => ("Telegram start requested. Check connection status before sending a message.", Some(Next::Route)),
                    Plan::Service(_) => ("Telegram stop requested. Check connection status to confirm it stopped.", None),
                    Plan::Route(_) => ("Destination saved. Replies require a matching installed hook connected to this chat.", None),
                    Plan::Binding(_) => ("Reply hook recipient saved. Replies remain tied to the selected terminal and conversation.", None),
                };
                let approval = plan.approval();
                let Reply::Data(result) = source.telegram(Request::Apply { plan, approval })?
                else {
                    return Err("Invalid Telegram result".into());
                };
                if matches!(next, Some(Next::Token))
                    && let Ok(Reply::Data(status)) = source.telegram(Request::Inspect)
                    && status["token_locally_valid"] == true
                {
                    next = Some(Next::Start);
                }
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
                    json!({"text": format!("{summary}\n\n{}{}", super::dialog_text::review(&result), next.map(|step| format!("\n\nNext: {}. Close this summary to continue or finish later.", step.label())).unwrap_or_default())}),
                    Stage::Output(next),
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
    fn starting_an_unconfigured_connection_opens_setup_without_starting_service() {
        let (mut menu, mut local, mut seq) = (Menu::default(), Local::new(), 0);
        let result = menu
            .start("telegram.start", &mut local, &config(), &mut seq)
            .unwrap();
        assert_eq!(result.frames[0]["title"], "Instance name");
        assert_eq!(local.applied, 0);
        assert!(!local.directory.join("telegram-service.json").exists());
        assert!(!local.directory.join("telegram-process.json").exists());
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
        answer(&mut menu, &mut local, &mut seq, json!("later"));
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
    #[test]
    fn invalid_identity_keeps_prior_fields_and_allows_correction() {
        let (mut menu, mut local, mut seq) = (Menu::default(), Local::new(), 0);
        menu.start("telegram.details", &mut local, &config(), &mut seq)
            .unwrap();
        let invalid = answer(&mut menu, &mut local, &mut seq, json!("bad name"));
        assert_eq!(invalid.frames[0]["title"], "Instance name");
        answer(&mut menu, &mut local, &mut seq, json!("fixture"));
        let invalid = answer(&mut menu, &mut local, &mut seq, json!("@mybot"));
        assert_eq!(invalid.frames[0]["title"], "Telegram bot ID");
        assert!(
            invalid.frames[0]["description"]
                .as_str()
                .unwrap()
                .contains("positive numeric ID")
        );
        for text in ["123", "456", "-100789", "apply"] {
            answer(&mut menu, &mut local, &mut seq, json!(text));
        }
        let saved = ServiceConfig::load(&local.directory.join("telegram-service.json")).unwrap();
        assert_eq!(saved.instance, "fixture");
        assert_eq!(saved.bot_id, 123);
        assert_eq!(saved.grants[0].chat, -100789);
        assert_eq!(local.applied, 1);
    }

    #[test]
    fn guided_settings_continue_to_private_token_without_starting_service() {
        let (mut menu, mut local, mut seq) = (Menu::default(), Local::new(), 0);
        details(&mut menu, &mut local, &mut seq);
        let saved = answer(&mut menu, &mut local, &mut seq, json!("apply"));
        assert!(
            saved.frames[0]["text"]
                .as_str()
                .unwrap()
                .contains("does not start")
        );
        let next = answer(&mut menu, &mut local, &mut seq, json!("close"));
        assert_eq!(next.frames[0]["options"][0][1], "Save the bot token");
        let token = answer(&mut menu, &mut local, &mut seq, json!("continue"));
        assert_eq!(token.frames[0]["secret"], true);
        answer(&mut menu, &mut local, &mut seq, json!("123:fixture_secret"));
        let saved = answer(&mut menu, &mut local, &mut seq, json!("apply"));
        assert!(!format!("{saved:?}").contains("fixture_secret"));
        let next = answer(&mut menu, &mut local, &mut seq, json!("close"));
        assert_eq!(next.frames[0]["options"][0][1], "Review starting Telegram");
        answer(&mut menu, &mut local, &mut seq, json!("later"));
        assert!(!menu.pending());
        assert!(!local.directory.join("telegram-process.json").exists());
        assert_eq!(local.applied, 2);
    }

    #[test]
    fn starting_without_a_token_guides_to_token_entry() {
        let (mut menu, mut local, mut seq) = (Menu::default(), Local::new(), 0);
        details(&mut menu, &mut local, &mut seq);
        answer(&mut menu, &mut local, &mut seq, json!("apply"));
        menu.cancel();
        let output = menu
            .start("telegram.start", &mut local, &config(), &mut seq)
            .unwrap();
        assert_eq!(output.frames[0]["title"], "Telegram bot token");
        assert_eq!(output.frames[0]["secret"], true);
        assert_eq!(local.applied, 1);
        assert!(!local.directory.join("telegram-process.json").exists());
    }
    #[test]
    fn rejected_token_can_be_corrected_without_restarting_setup() {
        let (mut menu, mut local, mut seq) = (Menu::default(), Local::new(), 0);
        details(&mut menu, &mut local, &mut seq);
        answer(&mut menu, &mut local, &mut seq, json!("apply"));
        menu.cancel();
        menu.start("telegram.token", &mut local, &config(), &mut seq)
            .unwrap();
        let rejected = answer(&mut menu, &mut local, &mut seq, json!("999:fixture_secret"));
        assert_eq!(rejected.frames[0]["secret"], true);
        assert!(!format!("{rejected:?}").contains("fixture_secret"));
        assert!(!local.directory.join("telegram.env").exists());
        let corrected = answer(&mut menu, &mut local, &mut seq, json!("123:fixture_secret"));
        assert_eq!(corrected.frames[0]["title"], "Review Telegram changes");
        answer(&mut menu, &mut local, &mut seq, json!("cancel"));
        assert_eq!(local.applied, 1);
    }
}
