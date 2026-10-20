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
use zeroize::Zeroizing;
type Result<T> = std::result::Result<T, String>;
#[derive(Clone)]
struct Details {
    field: usize,
    expected: Option<ServiceConfig>,
    updated: ServiceConfig,
    actor: Option<i64>,
    automatic: bool,
}
#[derive(Clone, Copy)]
enum Next {
    Token,
    Discover,
    Start,
    Route,
}
impl Next {
    fn action(self) -> &'static str {
        match self {
            Self::Token => "telegram.token",
            Self::Discover => "telegram.discover",
            Self::Start => "telegram.start",
            Self::Route => "telegram.route",
        }
    }
    fn label(self) -> &'static str {
        match self {
            Self::Token => "Save the bot token",
            Self::Discover => "Find a DM or group chat",
            Self::Start => "Review starting Telegram",
            Self::Route => "Choose a destination terminal",
        }
    }
    fn help(self) -> &'static str {
        match self {
            Self::Token => {
                "Get your bot token from @BotFather in Telegram. It is stored in your private configuration directory."
            }
            Self::Discover => {
                "Send /start to your bot in a DM, or /start@YourBot in a group, then refresh the discovery list. Select an exact chat and review the sender grant."
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
    LocalSetup(String),
    Settings(ServiceConfig),
    Names(Vec<Value>),
    Name(i64),
    Owner {
        updated: ServiceConfig,
        token: Zeroizing<String>,
    },
    Output(Option<Next>),
    Continue(Next),
    Details(Box<Details>),
    InitialMode(ServiceConfig),
    AutomaticToken(ServiceConfig, bool),
    AutomaticName {
        updated: ServiceConfig,
        token: Zeroizing<String>,
    },
    AutomaticReview {
        plan: Box<Plan>,
        token: Zeroizing<String>,
    },
    Discovered {
        candidates: Vec<Value>,
        expected: ServiceConfig,
    },
    ChatKind(Box<Details>),
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
    InstallHooks {
        identity: crate::core::identity::TerminalId,
        chat: i64,
        actor: i64,
    },
    Confirm(Box<Plan>),
}
struct Pending {
    id: u64,
    mode: &'static str,
    choices: Vec<String>,
    stage: Stage,
    frame: Value,
}
#[derive(Default)]
pub struct Menu {
    pending: Option<Pending>,
    target: Option<crate::core::identity::TerminalId>,
    hooks: super::hook_menu::Menu,
}
pub fn available(action: &str) -> bool {
    matches!(
        action,
        "telegram.details"
            | "telegram.token"
            | "telegram.start"
            | "telegram.stop"
            | "telegram.route"
            | "telegram.discover"
    )
}
impl Menu {
    pub fn pending(&self) -> bool {
        self.pending.is_some() || self.hooks.pending()
    }
    pub fn cancel(&mut self) {
        self.pending = None;
        self.target = None;
        self.hooks.cancel();
    }
    pub fn for_pane<S: Source>(
        &mut self,
        pane: &Value,
        source: &mut S,
        config: &ControlConfig,
        seq: &mut u64,
    ) -> Result<Outcome> {
        let target = crate::adapters::terminals::identity(pane)?;
        let outcome = self.start("telegram.route", source, config, seq)?;
        self.target = Some(target);
        Ok(outcome)
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
            frame: frame.clone(),
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
        self.target = None;
        let Reply::Data(local) = source.telegram(Request::Inspect)? else {
            return Err("Invalid Telegram settings response".into());
        };
        if local["local_configured"] == false {
            return self.frame(seq,"choose","Save local configuration",json!({"description":"Telegram needs a saved local configuration. Save this instance first, then continue connecting Telegram. Existing configuration is preserved; no tools are installed.","options":[["save","Save and continue","Create private local settings"],["cancel","Cancel","Leave files unchanged"]]}),Stage::LocalSetup(action.into()));
        }
        if local["configured"] == false
            && matches!(
                action,
                "telegram.route" | "telegram.discover" | "telegram.token"
            )
        {
            return self.start("telegram.details", source, _config, seq);
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
            "telegram.discover" => {
                let Reply::Data(data) = source.telegram(Request::Discover)? else { return Err("Invalid Telegram discovery response".into()); };
                let candidates = data["chats"].as_array().cloned().ok_or("Invalid Telegram chat candidates")?;
                let Reply::Data(status) = source.telegram(Request::Inspect)? else { return Err("Invalid Telegram settings response".into()); };
                let expected: ServiceConfig = serde_json::from_value(status["settings"].clone()).map_err(|_| "Configure Telegram first")?;
                if candidates.is_empty() {
                    return self.frame(seq,"output","No recent chats found",json!({"text":"Send /start to the bot in a DM, or /start@YourBot in a group. Keep the Daedalus Telegram service stopped while discovering, then retry. No grant was created."}),Stage::Output(None));
                }
                let options = candidates.iter().enumerate().map(|(i,c)|json!([i.to_string(),format!("{} · {}",c["title"].as_str().unwrap_or("Chat"),c["chat"]),format!("{} · sender {}",c["kind"].as_str().unwrap_or("chat"),c["actor"])] )).collect::<Vec<_>>();
                self.frame(seq,"choose","Review discovered chats",json!({"description":"Only local approval grants a sender access. These candidates came from recent bot updates; no route is inferred.","options":options}),Stage::Discovered{candidates,expected})
            }
            "telegram.route" => {
                let Reply::Data(data)=source.telegram(Request::Routes)? else {return Err("Invalid route inventory".into());};
                let choices=data["grants"].as_array().ok_or("Invalid route grants")?.iter().enumerate().map(|(i,g)| {
                    let current = data["terminals"].as_array().into_iter().flatten()
                        .find(|t| t["identity"] == g["route"]["terminal"]);
                    let destination = current.and_then(|t| t["label"].as_str()).unwrap_or_else(|| {
                        if g["route"].is_null() { "Not assigned" } else { "Saved terminal offline" }
                    });
                    json!([i.to_string(),format!("{} · {}",g["title"].as_str().unwrap_or("Chat"),g["chat"]),format!("{destination} · authorized user {}",g["actor"])])
                }).collect::<Vec<_>>();
                if choices.is_empty(){return self.start("telegram.details", source, _config, seq);}
                self.frame(seq,"choose","Choose Telegram chat",json!({"options":choices,"description":"Each chat has its own saved default terminal. Changing one chat does not change other chats."}),Stage::RouteGrant(data))
            }
            "telegram.token" => self.frame(seq,"input","Telegram bot token",json!({"secret":true,"description":"Paste the complete token from @BotFather (numeric ID:secret). It must match your configured bot ID. It is stored privately and never shown in the review."}),Stage::Token),
            "telegram.start"|"telegram.stop" => self.review(seq,source.telegram(Request::Service{start:action=="telegram.start"})?),
            "telegram.details" => {
                let Reply::Data(value)=source.telegram(Request::Inspect)? else {return Err("Invalid Telegram settings response".into());};
                let expected:Option<ServiceConfig>=value.get("settings").map(|v|serde_json::from_value(v.clone()).map_err(|_|"Invalid Telegram settings")).transpose()?;
                let directory=PathBuf::from(value["config_directory"].as_str().ok_or("Missing configuration directory")?);
                let updated=expected.clone().unwrap_or(ServiceConfig {version:1,instance:String::new(),bot_id:0,token_file:directory.join("telegram.env"),grants:vec![],listen_chats:BTreeSet::new(),registrations:vec![],hook_bindings:vec![]});
                if expected.is_none() {
                    self.frame(seq,"choose","Connect Telegram",json!({"description":"Start with your private DM. You can add named groups afterwards.","options":[["dm","Connect my bot and DM","Enter the bot token and your Telegram user ID"],["automatic","Find my DM or group","Discover recent chats, then review access"],["manual","Advanced setup","Enter bot and chat IDs separately"]]}),Stage::InitialMode(updated))
                } else {
                    self.frame(seq,"choose","Telegram configuration",json!({"description":"Edit this connection without repeating onboarding. Existing chats, routes and hooks are preserved.","options":[["add","Add a DM or group","Authorize a Telegram user and chat ID"],["names","Manage chat names","Name registered chats for the pane selector"],["route","Assign chats to panes","Choose a default destination for each chat"],["token","Change bot token","Replace the private credential for this bot"],["discover","Find chats automatically","Discover recent chats while the service is stopped"],["start","Start connection","Review starting the Telegram service"],["stop","Stop connection","Stop this instance's managed service"]]}),Stage::Settings(updated))
                }
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
        automatic: bool,
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
                "Enter the group's numeric chat ID, including its leading minus sign. Existing chat permissions are preserved."
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
                automatic,
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
        if self.hooks.pending() {
            return self.hooks.reply(request, source, _config, seq);
        }
        let pending = self.pending.as_ref().ok_or("No Telegram dialog pending")?;
        if request["op"] != "reply" || request["id"].as_u64() != Some(pending.id) {
            return Err("Unexpected Telegram reply".into());
        }
        let value = request.get("value").ok_or("Missing Telegram reply")?;
        if !value.is_null() {
            let text = value.as_str().ok_or("Invalid Telegram reply")?;
            if pending.mode == "input"
                && (text.is_empty() || text.len() > 1024 || text.chars().any(char::is_control))
            {
                let mut frame = pending.frame.clone();
                let hint = if text.is_empty() {
                    "This field is empty. Paste or type a value, then press Enter."
                } else {
                    "Use at most 1024 characters without control characters."
                };
                frame["description"] = json!(format!(
                    "{hint}\n\n{}",
                    frame["description"].as_str().unwrap_or("")
                ));
                *seq = seq.checked_add(1).ok_or("Dialog sequence exhausted")?;
                frame["id"] = json!(*seq);
                self.pending.as_mut().unwrap().id = *seq;
                return Ok(Outcome {
                    frames: vec![frame],
                    ..Outcome::default()
                });
            }
            if (pending.mode == "output" && text != "close")
                || (pending.mode == "choose" && !pending.choices.iter().any(|v| v == text))
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
                3 if text.parse::<i64>().ok().filter(|id| *id < 0).is_none() => {
                    Some("Enter a negative numeric group chat ID, including its minus sign.")
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
                    details.automatic,
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
            Stage::LocalSetup(action) => {
                if text == "cancel" { return Ok(Outcome::default()); }
                let configuration = source.finish_local_setup(_config)?;
                let mut outcome = self.start(&action, source, &configuration, seq)?;
                outcome.configuration = Some(configuration);
                outcome.refresh = true;
                Ok(outcome)
            }
            Stage::Settings(updated) => match text {
                "add" => self.field(seq,2,Some(updated.clone()),updated,None,false),
                "names" => {
                    let Reply::Data(data) = source.telegram(Request::Inspect)? else { return Err("Invalid chat inventory".into()); };
                    let mut seen = BTreeSet::new();
                    let chats = data["chats"].as_array().ok_or("Invalid chats")?.iter().filter(|g| seen.insert(g["chat"].as_i64())).cloned().collect::<Vec<_>>();
                    if chats.is_empty() { return self.field(seq,2,Some(updated.clone()),updated,None,false); }
                    let options = chats.iter().enumerate().map(|(i,g)|json!([i.to_string(),g["title"],format!("Chat ID {}",g["chat"])])).collect::<Vec<_>>();
                    self.frame(seq,"choose","Manage chat names",json!({"options":options}),Stage::Names(chats))
                }
                other => self.start(match other {"route"=>"telegram.route","token"=>"telegram.token","discover"=>"telegram.discover","start"=>"telegram.start",_=>"telegram.stop"},source,_config,seq),
            },
            Stage::Names(chats) => {
                let chat = chats.get(text.parse::<usize>().map_err(|_|"Invalid chat")?).and_then(|c|c["chat"].as_i64()).ok_or("Invalid chat")?;
                self.frame(seq,"input","Chat display name",json!({"description":"Use a short name for this chat (up to 120 characters). This is private display metadata; chat ID and permissions stay unchanged."}),Stage::Name(chat))
            }
            Stage::Name(chat) => self.review(seq,source.telegram(Request::NameChat { chat, title:text.into() })?),
            Stage::Owner { mut updated, token } => {
                let Some(owner) = text.parse::<i64>().ok().filter(|id| *id > 0) else {
                    return self.frame(seq,"input","Telegram owner user ID",json!({"description":"Enter your positive numeric Telegram user ID, not an @username."}),Stage::Owner { updated, token });
                };
                updated.instance = "telegram".into();
                updated.grants.push(Grant { chat:owner, actor:owner, permissions:vec![Access::Read,Access::Send,Access::Manage] });
                let Reply::Plan(plan) = source.telegram(Request::Configure { expected:None, config:Box::new(updated) })? else { return Err("Invalid Telegram setup plan".into()); };
                self.frame(seq,"choose","Review Telegram DM connection",json!({"description":super::dialog_text::format(&plan.review()).map_err(|_|"Cannot display review")?,"options":[["apply","Save connection","Save the owner DM, then review private token storage"],["cancel","Cancel","Leave configuration unchanged"]]}),Stage::AutomaticReview { plan, token })
            }
            Stage::InitialMode(updated) => {
                if text == "automatic" || text == "dm" {
                    self.frame(seq,"input","Telegram bot token",json!({"secret":true,"description":"Paste the complete token from @BotFather. Its numeric prefix identifies your bot; the token stays in private local storage and is never shown in a review."}),Stage::AutomaticToken(updated, text == "dm"))
                } else {
                    self.field(seq,0,None,updated,None,false)
                }
            }
            Stage::AutomaticToken(mut updated, dm) => {
                let bot = text.split_once(':').and_then(|(id,_)|id.parse::<i64>().ok()).filter(|id|*id>0);
                let Some(bot) = bot.filter(|bot| super::telegram_config::validate_token(*bot,text.as_bytes()).is_ok()) else {
                    return self.frame(seq,"input","Telegram bot token",json!({"secret":true,"description":"Paste the complete token from @BotFather (numeric ID:secret). No token was saved."}),Stage::AutomaticToken(updated,dm));
                };
                updated.bot_id=bot;
                if dm {
                    return self.frame(seq,"input","Telegram owner user ID",json!({"description":"Enter your numeric Telegram user ID. Your private DM is the default chat; groups can be added later."}),Stage::Owner { updated, token:Zeroizing::new(text.to_owned()) });
                }
                self.frame(seq,"input","Connection name",json!({"description":"Choose a local name for this Telegram connection (letters, numbers, underscores or hyphens)."}),Stage::AutomaticName{updated,token:Zeroizing::new(text.to_owned())})
            }
            Stage::AutomaticName { mut updated, token } => {
                if text.len()>64 || !text.bytes().all(|b|b.is_ascii_alphanumeric()||b"_-".contains(&b)) {
                    return self.frame(seq,"input","Connection name",json!({"description":"Use 1–64 letters, numbers, underscores or hyphens."}),Stage::AutomaticName{updated,token});
                }
                updated.instance=text.into();
                let Reply::Plan(plan)=source.telegram(Request::Configure{expected:None,config:Box::new(updated)})? else {return Err("Invalid Telegram setup plan".into())};
                self.frame(seq,"choose","Review Telegram changes",json!({"description":super::dialog_text::format(&plan.review()).map_err(|_|"Cannot display Telegram review")?,"options":[["apply","Save bot identity","The token is saved separately after this review"],["cancel","Cancel","Leave configuration unchanged"]]}),Stage::AutomaticReview{plan,token})
            }
            Stage::AutomaticReview { plan, token } => {
                if text=="cancel" {return Ok(Outcome::default());}
                let approval=plan.approval();
                let Reply::Data(_)=source.telegram(Request::Apply{plan,approval})? else {return Err("Invalid Telegram setup result".into())};
                self.review(seq,source.telegram(Request::Token{value:token.to_string()})?)
            }
            Stage::Discovered { candidates, expected } => {
                let index=text.parse::<usize>().map_err(|_|"Invalid chat selection")?;
                let candidate=candidates.get(index).ok_or("Chat selection changed")?;
                let actor=candidate["actor"].as_i64().ok_or("Missing sender ID")?;
                let chat=candidate["chat"].as_i64().ok_or("Missing chat ID")?;
                let mut updated=expected.clone();
                updated.grants.retain(|g|g.chat!=chat || g.actor!=actor);
                updated.grants.push(Grant{chat,actor,permissions:vec![Access::Read,Access::Send,Access::Manage]});
                self.review(seq,source.telegram(Request::Configure{expected:Some(Box::new(expected)),config:Box::new(updated)})?)
            }
            Stage::InstallHooks { identity, chat, actor } => {
                if text == "later" { return Ok(Outcome::default()); }
                let observed = source.refresh(_config)?;
                let panes = observed.snapshot["panes"].as_array().ok_or("Missing live pane inventory")?;
                let matches = panes.iter().filter(|p| crate::adapters::terminals::identity(p).is_ok_and(|id| id == identity)).collect::<Vec<_>>();
                if matches.len() != 1 { return Err("Destination terminal changed. Refresh and select it again.".into()); }
                self.hooks.start_defaults(matches[0].clone(), (chat, actor), source, _config, seq)
            }
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
                if let Some(identity) = self.target.take() {
                    let matches = terminals.iter().filter(|t| t.available && t.identity == identity).collect::<Vec<_>>();
                    if matches.len() != 1 {
                        return Err("This terminal is no longer available. Refresh and select it again.".into());
                    }
                    return self.review(seq, source.telegram(Request::Route {
                        chat,
                        actor,
                        target: Box::new(matches[0].clone()),
                    })?);
                }
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
            Stage::ChatKind(details) => {
                if text == "dm" {
                    let mut updated = details.updated.clone();
                    let actor = details.actor.ok_or("Missing authorized user")?;
                    updated.grants.retain(|g| g.actor != actor || g.chat != actor);
                    updated.grants.push(Grant { chat: actor, actor, permissions: vec![Access::Read, Access::Send, Access::Manage] });
                    self.review(seq, source.telegram(Request::Configure {
                        expected: details.expected.map(Box::new),
                        config: Box::new(updated),
                    })?)
                } else {
                    self.field(seq, 3, details.expected, details.updated, details.actor, false)
                }
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
                        serde_json::from_value::<crate::core::routes::Terminal>(review["terminal"].clone()).map_err(|_| "Invalid route target")?.identity,
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
                    Plan::ChatName(_) => ("Chat name saved. It will appear when assigning a Telegram chat to a pane.", None),
                    Plan::Configuration(_) => ("Telegram settings saved. Saving settings does not start or reconnect the bot.", Some(Next::Token)),
                    Plan::Token(_) => ("Bot token saved privately. Its format matches the configured bot; Telegram has not verified it yet.", Some(Next::Start)),
                    Plan::Service(_) if plan.review()["operation"] == "start" => ("Telegram start requested. Check connection status before sending a message.", Some(Next::Route)),
                    Plan::Service(_) => ("Telegram stop requested. Check connection status to confirm it stopped.", None),
                    Plan::Route(_) => ("Destination saved. Replies require a matching installed hook connected to this chat.", None),
                    Plan::Binding(_) => ("Reply hook recipient saved. Replies remain tied to the selected terminal and conversation.", None),
                };
                let token_plan = matches!(&*plan, Plan::Token(_));
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
                if token_plan
                    && let Ok(Reply::Data(status)) = source.telegram(Request::Inspect)
                    && status["settings"]["grants"].as_array().is_some_and(Vec::is_empty)
                {
                    next = Some(Next::Discover);
                }
                if let Some((identity, chat, actor)) = route {
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
                    let mut outcome = self.frame(seq, "choose", "Destination saved · hooks required", json!({
                        "description":"This terminal has no matching Telegram hook registration. Install standard hooks, then connect the chat in the installer. The harness may require native trust approval or a reload. The Telegram service must also be running; groups receive ordinary messages only when listening is enabled.",
                        "options":[["install","Set up standard hooks","Detect the native profile and review changes"],["later","Finish later","The saved route is not a verified connection"]]
                    }), Stage::InstallHooks { identity, chat, actor })?;
                    outcome.refresh = true;
                    return Ok(outcome);
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
                    automatic,
                } = *details;
                match field {
                    0 => updated.instance = text.into(),
                    1 => {
                        updated.bot_id = text
                            .parse::<i64>()
                            .ok()
                            .filter(|v| *v > 0)
                            .ok_or("Bot ID must be positive")?;
                        if automatic {
                            return self.review(seq,source.telegram(Request::Configure{expected:None,config:Box::new(updated)})?);
                        }
                    }
                    2 => {
                        actor = Some(
                            text.parse::<i64>()
                                .ok()
                                .filter(|v| *v > 0)
                                .ok_or("User ID must be positive")?,
                        );
                        return self.frame(seq, "choose", "Where will you use this bot?", json!({
                            "description": "A DM uses your user ID as its chat ID. A group needs its own negative chat ID. In groups, mention or reply to the bot by default; /listen also needs BotFather privacy mode disabled or bot admin rights for all messages. You can add another chat later.",
                            "options": [["dm", "Direct message", "Use this user ID for the private chat"], ["group", "Group chat", "Enter the group's numeric chat ID"]]
                        }), Stage::ChatKind(Box::new(Details { field: 3, expected, updated, actor, automatic })));
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
                self.field(seq, field + 1, expected, updated, actor, automatic)
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
    struct Routing {
        inventory: Value,
        selected: Option<(i64, crate::core::identity::TerminalId)>,
    }
    impl Source for Routing {
        fn telegram(&mut self, request: Request) -> Result<Reply> {
            match request {
                Request::Inspect => Ok(Reply::Data(json!({}))),
                Request::Routes => Ok(Reply::Data(self.inventory.clone())),
                Request::Route { chat, target, .. } => {
                    self.selected = Some((chat, target.identity));
                    Err("Reached review without applying".into())
                }
                _ => panic!("Unexpected routing operation"),
            }
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
    #[test]
    fn pane_chat_button_keeps_exact_target_and_rejects_a_replaced_terminal() {
        let pane = json!({"backend":"herdr","session":"fixture","pane_id":"pane-a","terminal_id":"generation-a"});
        let identity = crate::adapters::terminals::identity(&pane).unwrap();
        let terminal = crate::core::routes::Terminal {
            identity: identity.clone(),
            label: "Selected agent".into(),
            workspace: "Project".into(),
            harness: "codex".into(),
            conversation: Some("session".into()),
            available: true,
        };
        let mut source = Routing {
            inventory: json!({"grants":[{"chat":-100,"actor":42,"title":"Team"}],"terminals":[terminal]}),
            selected: None,
        };
        let mut sessions = crate::app::session_menu::SessionMenu::default();
        let mut seq = 0;
        sessions
            .start(
                "telegram.route",
                crate::app::session_menu::View {
                    snapshot: json!({"panes":[pane.clone()]}),
                    health: json!({}),
                    layouts: Layouts::default(),
                    selected: Some(pane.clone()),
                    checkpoint: None,
                    node: "selected-node".into(),
                },
                &mut source,
                &config(),
                &mut seq,
            )
            .unwrap();
        let reply = json!({"op":"reply","id":seq,"value":"0"});
        assert!(
            sessions
                .reply(&reply, &mut source, &config(), &mut seq)
                .unwrap_err()
                .contains("Reached review")
        );
        assert_eq!(source.selected, Some((-100, identity)));
        source.selected = None;
        source.inventory["terminals"][0]["identity"]["generation"] = json!("replacement");
        let mut menu = Menu::default();
        menu.for_pane(&pane, &mut source, &config(), &mut seq)
            .unwrap();
        let reply = json!({"op":"reply","id":seq,"value":"0"});
        assert!(
            menu.answer(&mut source, &config(), &reply, &mut seq)
                .is_err()
        );
        assert!(source.selected.is_none());
    }
    #[test]
    fn missing_hooks_continue_on_exact_pane_and_cancel_nested_dialog() {
        struct Inventory(Value);
        impl Source for Inventory {
            fn refresh(&mut self, _: &ControlConfig) -> Result<Observations> {
                Ok(Observations {
                    snapshot: self.0.clone(),
                    ..Default::default()
                })
            }
            fn preview(&mut self, _: &Value, _: usize) -> Result<String> {
                unreachable!()
            }
            fn save_layouts(&mut self, _: &Layouts) -> Result<()> {
                unreachable!()
            }
        }
        let pane = json!({"backend":"herdr","session":"fixture","pane_id":"p1","terminal_id":"original","agent":"codex"});
        let identity = crate::adapters::terminals::identity(&pane).unwrap();
        for replaced in [false, true] {
            let mut current = pane.clone();
            if replaced {
                current["terminal_id"] = json!("replacement");
            }
            let mut source = Inventory(json!({"panes":[current]}));
            let mut menu = Menu::default();
            let mut seq = 0;
            menu.frame(
                &mut seq,
                "choose",
                "Install",
                json!({"options":[["install","Install","Review"]]}),
                Stage::InstallHooks {
                    identity: identity.clone(),
                    chat: -100,
                    actor: 1,
                },
            )
            .unwrap();
            let outcome = menu.answer(
                &mut source,
                &config(),
                &json!({"op":"reply","id":seq,"value":"install"}),
                &mut seq,
            );
            if replaced {
                assert!(
                    outcome
                        .unwrap_err()
                        .contains("Destination terminal changed")
                );
            } else {
                assert_eq!(
                    outcome.unwrap().frames[0]["title"],
                    "Native hook profile file (absolute path)"
                );
                assert!(menu.pending());
                menu.cancel();
                assert!(!menu.pending());
            }
        }
    }
    struct Local {
        directory: PathBuf,
        applied: usize,
        require_local: bool,
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
                require_local: false,
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
            let inspect = matches!(&request, Request::Inspect);
            let mut reply = super::super::telegram_admin::execute(&self.directory, request)?;
            if inspect
                && self.require_local
                && let Reply::Data(value) = &mut reply
            {
                value["local_configured"] = json!(self.directory.join("control.json").exists());
            }
            Ok(reply)
        }
        fn finish_local_setup(&mut self, config: &ControlConfig) -> Result<ControlConfig> {
            if let Some(bytes) = layouts::read_private(&self.directory.join("control.json"))? {
                return ControlConfig::parse(&bytes);
            }
            super::super::setup::SetupPlan::fresh(&self.directory, config.clone())?.apply()?;
            Ok(config.clone())
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
        answer(menu, local, seq, json!("manual"));
        for text in ["fixture", "123", "456", "group", "-100789"] {
            answer(menu, local, seq, json!(text));
        }
    }
    #[test]
    fn quick_connection_derives_bot_id_defaults_to_owner_dm_and_redacts_token() {
        let (mut menu, mut local, mut seq) = (Menu::default(), Local::new(), 0);
        menu.start("telegram.details", &mut local, &config(), &mut seq)
            .unwrap();
        answer(&mut menu, &mut local, &mut seq, json!("dm"));
        let retry = answer(&mut menu, &mut local, &mut seq, json!("invalid"));
        assert_eq!(retry.frames[0]["secret"], true);
        answer(&mut menu, &mut local, &mut seq, json!("123:fixture_secret"));
        let retry = answer(&mut menu, &mut local, &mut seq, json!("@owner"));
        assert_eq!(retry.frames[0]["title"], "Telegram owner user ID");
        let review = answer(&mut menu, &mut local, &mut seq, json!("456"));
        assert!(!format!("{review:?}").contains("fixture_secret"));
        assert!(!local.directory.join("telegram-service.json").exists());
        let token_review = answer(&mut menu, &mut local, &mut seq, json!("apply"));
        assert!(!format!("{token_review:?}").contains("fixture_secret"));
        answer(&mut menu, &mut local, &mut seq, json!("apply"));
        let saved = ServiceConfig::load(&local.directory.join("telegram-service.json")).unwrap();
        assert_eq!(saved.bot_id, 123);
        assert_eq!((saved.grants[0].chat, saved.grants[0].actor), (456, 456));
        assert_eq!(saved.read_token().unwrap(), "123:fixture_secret");
        assert!(!local.directory.join("telegram-process.json").exists());
        menu.cancel();
        let settings = menu
            .start("telegram.details", &mut local, &config(), &mut seq)
            .unwrap();
        assert_eq!(settings.frames[0]["title"], "Telegram configuration");
        answer(&mut menu, &mut local, &mut seq, json!("names"));
        answer(&mut menu, &mut local, &mut seq, json!("0"));
        answer(&mut menu, &mut local, &mut seq, json!("Personal DM"));
        answer(&mut menu, &mut local, &mut seq, json!("apply"));
        let Reply::Data(status) = local.telegram(Request::Inspect).unwrap() else {
            panic!()
        };
        assert_eq!(status["chats"][0]["title"], "Personal DM");
        assert_eq!(
            ServiceConfig::load(&local.directory.join("telegram-service.json"))
                .unwrap()
                .read_token()
                .unwrap(),
            "123:fixture_secret"
        );
    }
    #[test]
    fn telegram_setup_saves_local_configuration_only_after_confirmation() {
        let (mut menu, mut local, mut seq) = (Menu::default(), Local::new(), 0);
        local.require_local = true;
        let result = menu
            .start("telegram.details", &mut local, &config(), &mut seq)
            .unwrap();
        assert_eq!(result.frames[0]["title"], "Save local configuration");
        assert!(!local.directory.join("control.json").exists());
        answer(&mut menu, &mut local, &mut seq, json!("cancel"));
        assert!(!local.directory.join("control.json").exists());
        menu.start("telegram.details", &mut local, &config(), &mut seq)
            .unwrap();
        let next = answer(&mut menu, &mut local, &mut seq, json!("save"));
        assert!(next.configuration.is_some());
        assert_eq!(next.frames[0]["title"], "Connect Telegram");
        assert!(local.directory.join("control.json").exists());
        assert!(!local.directory.join("telegram-service.json").exists());
    }
    #[test]
    fn starting_an_unconfigured_connection_opens_setup_without_starting_service() {
        let (mut menu, mut local, mut seq) = (Menu::default(), Local::new(), 0);
        let result = menu
            .start("telegram.start", &mut local, &config(), &mut seq)
            .unwrap();
        assert_eq!(result.frames[0]["title"], "Connect Telegram");
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
    fn direct_message_uses_actor_as_chat_without_second_id() {
        let (mut menu, mut local, mut seq) = (Menu::default(), Local::new(), 0);
        menu.start("telegram.details", &mut local, &config(), &mut seq)
            .unwrap();
        answer(&mut menu, &mut local, &mut seq, json!("manual"));
        for text in ["fixture", "123", "456"] {
            answer(&mut menu, &mut local, &mut seq, json!(text));
        }
        let choice = answer(&mut menu, &mut local, &mut seq, json!("dm"));
        assert_eq!(choice.frames[0]["title"], "Review Telegram changes");
        answer(&mut menu, &mut local, &mut seq, json!("apply"));
        let saved = ServiceConfig::load(&local.directory.join("telegram-service.json")).unwrap();
        assert_eq!(saved.grants[0].actor, 456);
        assert_eq!(saved.grants[0].chat, 456);
    }
    #[test]
    fn automatic_setup_saves_no_grant_before_discovery() {
        let (mut menu, mut local, mut seq) = (Menu::default(), Local::new(), 0);
        let mode = menu
            .start("telegram.details", &mut local, &config(), &mut seq)
            .unwrap();
        assert_eq!(mode.frames[0]["title"], "Connect Telegram");
        let token = answer(&mut menu, &mut local, &mut seq, json!("automatic"));
        assert_eq!(token.frames[0]["secret"], true);
        answer(&mut menu, &mut local, &mut seq, json!("123:fixture_secret"));
        let review = answer(&mut menu, &mut local, &mut seq, json!("fixture"));
        assert_eq!(review.frames[0]["title"], "Review Telegram changes");
        let token_review = answer(&mut menu, &mut local, &mut seq, json!("apply"));
        assert!(!format!("{token_review:?}").contains("fixture_secret"));
        let saved = ServiceConfig::load(&local.directory.join("telegram-service.json")).unwrap();
        assert!(saved.grants.is_empty());
        assert!(!saved.token_file.exists());
        answer(&mut menu, &mut local, &mut seq, json!("apply"));
        let next = answer(&mut menu, &mut local, &mut seq, json!("close"));
        assert_eq!(next.frames[0]["options"][0][1], "Find a DM or group chat");
    }
    #[test]
    fn group_choice_rejects_a_private_chat_id() {
        let (mut menu, mut local, mut seq) = (Menu::default(), Local::new(), 0);
        menu.start("telegram.details", &mut local, &config(), &mut seq)
            .unwrap();
        answer(&mut menu, &mut local, &mut seq, json!("manual"));
        for text in ["fixture", "123", "456", "group"] {
            answer(&mut menu, &mut local, &mut seq, json!(text));
        }
        let retry = answer(&mut menu, &mut local, &mut seq, json!("456"));
        assert_eq!(retry.frames[0]["title"], "Telegram chat ID");
        assert_eq!(local.applied, 0);
    }
    #[test]
    fn adding_a_group_preserves_the_existing_dm() {
        let (mut menu, mut local, mut seq) = (Menu::default(), Local::new(), 0);
        menu.start("telegram.details", &mut local, &config(), &mut seq)
            .unwrap();
        answer(&mut menu, &mut local, &mut seq, json!("manual"));
        for text in ["fixture", "123", "456", "dm", "apply"] {
            answer(&mut menu, &mut local, &mut seq, json!(text));
        }
        menu.cancel();
        menu.start("telegram.details", &mut local, &config(), &mut seq)
            .unwrap();
        for text in ["add", "456", "group", "-100789", "apply"] {
            answer(&mut menu, &mut local, &mut seq, json!(text));
        }
        let saved = ServiceConfig::load(&local.directory.join("telegram-service.json")).unwrap();
        assert_eq!(saved.grants.len(), 2);
        assert!(saved.grants.iter().any(|g| g.chat == 456 && g.actor == 456));
        assert!(
            saved
                .grants
                .iter()
                .any(|g| g.chat == -100789 && g.actor == 456)
        );
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
        answer(&mut menu, &mut local, &mut seq, json!("manual"));
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
        for text in ["123", "456", "group", "-100789", "apply"] {
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
        let previous = seq;
        let empty = answer(&mut menu, &mut local, &mut seq, json!(""));
        assert_eq!(empty.frames[0]["secret"], true);
        assert!(
            empty.frames[0]["description"]
                .as_str()
                .unwrap()
                .contains("field is empty")
        );
        assert!(seq > previous);
        let too_long = answer(&mut menu, &mut local, &mut seq, json!("x".repeat(1025)));
        assert_eq!(too_long.frames[0]["title"], "Telegram bot token");
        assert!(!format!("{too_long:?}").contains(&"x".repeat(1025)));
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
