//! Reviewed local Telegram administration. Secrets never enter menu responses.
use super::telegram_config::{self, ServiceConfig};
use crate::adapters::telegram::Api;
use crate::core::layouts;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
type Result<T> = std::result::Result<T, String>;

/// Intentionally not Debug/Serialize: this plan holds a submitted credential.
pub struct TokenPlan {
    directory: PathBuf,
    configuration: Vec<u8>,
    config: ServiceConfig,
    before: Option<Vec<u8>>,
    after: Vec<u8>,
}
impl TokenPlan {
    pub fn prepare(directory: &Path, submitted: &str) -> Result<Self> {
        let _lock = telegram_config::configuration_lock(directory)?;
        let path = directory.join("telegram-service.json");
        let configuration = telegram_config::read_private(&path, 1024 * 1024)?
            .ok_or("Configure Telegram bot identity before setting its token")?;
        let config = ServiceConfig::load(&path)?;
        let token = telegram_config::validate_token(config.bot_id, submitted.as_bytes())?;
        let before = telegram_config::read_private(&config.token_file, 1024)?;
        Ok(Self {
            directory: directory.into(),
            configuration,
            config,
            before,
            after: token.into_bytes(),
        })
    }
    pub fn approval(&self) -> String {
        let mut hash = Sha256::new();
        for bytes in [
            self.directory.as_os_str().as_encoded_bytes(),
            self.configuration.as_slice(),
            self.before.as_deref().unwrap_or_default(),
            self.after.as_slice(),
        ] {
            hash.update((bytes.len() as u64).to_le_bytes());
            hash.update(bytes);
        }
        format!("{:x}", hash.finalize())
    }
    pub fn review(&self) -> Value {
        json!({"operation":"telegram_token", "bot_id":self.config.bot_id,
            "replaces_existing":self.before.is_some(), "network_verified":false,
            "approval":self.approval(), "service_restart_required":true})
    }
    pub fn apply(self, approval: &str) -> Result<Value> {
        if approval != self.approval() {
            return Err("Token approval does not match the reviewed plan".into());
        }
        let _lock = telegram_config::configuration_lock(&self.directory)?;
        if telegram_config::read_private(&self.directory.join("cutover-retired.json"), 1024 * 1024)?
            .is_some()
        {
            return Err("This instance was retired; configure its destination instead".into());
        }
        if telegram_config::read_private(
            &self.directory.join("telegram-service.json"),
            1024 * 1024,
        )?
        .as_ref()
            != Some(&self.configuration)
            || telegram_config::read_private(&self.config.token_file, 1024)? != self.before
        {
            return Err("Telegram configuration or token changed; review again".into());
        }
        if let Some(before) = &self.before {
            let backup = self.directory.join(format!(
                "telegram-token.backup-{:x}",
                Sha256::digest(before)
            ));
            match telegram_config::read_private(&backup, 1024)? {
                Some(existing) if existing != *before => {
                    return Err("Token backup conflicts".into());
                }
                Some(_) => (),
                None => layouts::write_private(&backup, before)?,
            }
        }
        layouts::write_private(&self.config.token_file, &self.after)?;
        Ok(json!({"state":"saved", "network_verified":false, "service_restart_required":true}))
    }
}

pub fn inspect(directory: &Path) -> Result<Value> {
    let path = directory.join("telegram-service.json");
    if telegram_config::read_private(&path, 1024 * 1024)?.is_none() {
        return Ok(
            json!({"configured":false,"service":super::telegram_supervisor::inspect(directory)?}),
        );
    }
    let config = ServiceConfig::load(&path)?;
    Ok(json!({"configured":true, "configuration":config.preview(),
        "token_locally_valid":config.read_token().is_ok(), "network_verified":false,
        "service":super::telegram_supervisor::inspect(directory)?}))
}

pub enum Request {
    Inspect,
    Discover,
    Routes,
    RouteBindings {
        chat: i64,
        actor: i64,
    },
    Bindings {
        registration: PathBuf,
    },
    Bind {
        registration: PathBuf,
        chat: i64,
        actor: i64,
    },
    Route {
        chat: i64,
        actor: i64,
        target: Box<crate::core::routes::Terminal>,
    },
    Configure {
        expected: Option<Box<ServiceConfig>>,
        config: Box<ServiceConfig>,
    },
    Token {
        value: String,
    },
    Service {
        start: bool,
    },
    Apply {
        plan: Box<Plan>,
        approval: String,
    },
}
pub enum Reply {
    Data(Value),
    Plan(Box<Plan>),
}
pub enum Plan {
    Configuration(telegram_config::ServicePlan),
    Token(TokenPlan),
    Service(ServicePlan),
    Route(Box<RoutePlan>),
    Binding(Box<BindingPlan>),
}
pub struct ServicePlan {
    directory: PathBuf,
    configuration: Vec<u8>,
    start: bool,
    process: Option<super::telegram_supervisor::Record>,
}
impl Plan {
    pub fn approval(&self) -> String {
        match self {
            Self::Configuration(plan) => plan.approval_token(),
            Self::Token(plan) => plan.approval(),
            Self::Route(plan) => plan.approval(),
            Self::Binding(plan) => plan.approval(),
            Self::Service(plan) => format!(
                "{:x}",
                Sha256::digest(
                    serde_json::to_vec(&(
                        &plan.directory,
                        &plan.configuration,
                        plan.start,
                        &plan.process
                    ))
                    .expect("serializable service plan")
                )
            ),
        }
    }
    pub fn review(&self) -> Value {
        match self {
            Self::Configuration(plan) => plan.review(),
            Self::Token(plan) => plan.review(),
            Self::Route(plan) => plan.review(),
            Self::Binding(plan) => plan.review(),
            Self::Service(plan) => {
                json!({"operation":if plan.start {"start"} else {"stop"},"approval":self.approval(),"effect":"Requests a service transition; refresh status to observe the result."})
            }
        }
    }
}
pub fn execute(directory: &Path, request: Request) -> Result<Reply> {
    match request {
        Request::Discover => {
            if super::telegram_supervisor::inspect(directory)?.state != "stopped" {
                return Err("Stop this bot's managed service before discovering chats".into());
            }
            let config = ServiceConfig::load(&directory.join("telegram-service.json"))?;
            let token = config.read_token()?;
            let mut api = crate::adapters::telegram::BotApi::pairing(&token)
                .map_err(|_| "Cannot initialize Telegram verification")?;
            let found = discover_chats(&config, &mut api)?;
            let mut names = chat_names(directory, config.bot_id)?;
            for candidate in found["chats"].as_array().into_iter().flatten() {
                if let (Some(chat), Some(title)) =
                    (candidate["chat"].as_i64(), candidate["title"].as_str())
                {
                    names.insert(chat, title.to_owned());
                }
            }
            if names.len() > 1000 {
                return Err("Chat catalog exceeds limit".into());
            }
            layouts::write_private(&directory.join("telegram-chats.json"), &serde_json::to_vec(
                &json!({"bot_id":config.bot_id,"chats":names.into_iter().map(|(chat,title)|json!({"chat":chat,"title":title})).collect::<Vec<_>>()})
            ).map_err(|_| "Cannot encode chat catalog")?)?;
            Ok(Reply::Data(found))
        }
        Request::RouteBindings { chat, actor } => {
            let live = live_terminals(directory)?;
            Ok(Reply::Data(route_bindings(directory, chat, actor, &live)?))
        }
        Request::Bindings { registration } => {
            let config = ServiceConfig::load(&directory.join("telegram-service.json"))?;
            let live = live_terminals(directory)?;
            let mut choices = Vec::new();
            for grant in &config.grants {
                if !grant.permissions.contains(&telegram_config::Access::Send)
                    || !grant.permissions.contains(&telegram_config::Access::Read)
                {
                    continue;
                }
                if let Ok(plan) =
                    BindingPlan::prepare(directory, &registration, grant.chat, grant.actor, &live)
                {
                    choices.push(plan.review());
                }
            }
            Ok(Reply::Data(json!({"recipients":choices})))
        }
        Request::Bind {
            registration,
            chat,
            actor,
        } => Ok(Reply::Plan(Box::new(Plan::Binding(Box::new(
            BindingPlan::prepare(
                directory,
                &registration,
                chat,
                actor,
                &live_terminals(directory)?,
            )?,
        ))))),
        Request::Routes => {
            let config = ServiceConfig::load(&directory.join("telegram-service.json"))?;
            let terminals = live_terminals(directory)?;
            let names = chat_names(directory, config.bot_id)?;
            let owners = config
                .grants
                .iter()
                .filter(|g| {
                    g.chat == g.actor && g.permissions.contains(&telegram_config::Access::Manage)
                })
                .map(|g| g.actor)
                .collect::<std::collections::BTreeSet<_>>();
            let owner = (owners.len() == 1).then(|| *owners.first().unwrap());
            let grants = config
                .grants
                .iter()
                .filter(|g| g.permissions.contains(&telegram_config::Access::Manage))
                .filter(|g| {
                    !owner.is_some_and(|owner| {
                        g.actor != owner
                            && config.grants.iter().any(|other| {
                                other.chat == g.chat
                                    && other.actor == owner
                                    && other.permissions.contains(&telegram_config::Access::Manage)
                            })
                    })
                })
                .map(|grant| {
                    let scope = crate::core::routes::Scope {
                        instance: config.instance.clone(),
                        bot: config.bot_id.to_string(),
                        chat: grant.chat.to_string(),
                    };
                    let route = super::diagnostics::with_database(
                        &directory.join("routes.sqlite3"),
                        |con| {
                            crate::core::routes::read_route(con, &scope).map_err(|e| e.to_string())
                        },
                    )?
                    .flatten();
                    let mut value =
                        serde_json::to_value(grant).map_err(|_| "Invalid chat grant")?;
                    value["title"] = json!(names.get(&grant.chat).cloned().unwrap_or_else(|| {
                        if grant.chat > 0 {
                            "Direct message".into()
                        } else {
                            "Group".into()
                        }
                    }));
                    value["route"] =
                        serde_json::to_value(route).map_err(|_| "Invalid saved route")?;
                    Ok(value)
                })
                .collect::<Result<Vec<_>>>()?;
            Ok(Reply::Data(json!({"grants":grants,"terminals":terminals})))
        }
        Request::Route {
            chat,
            actor,
            target,
        } => Ok(Reply::Plan(Box::new(Plan::Route(Box::new(
            RoutePlan::prepare(directory, chat, actor, *target, &live_terminals(directory)?)?,
        ))))),
        Request::Inspect => {
            let mut value = inspect(directory)?;
            value["config_directory"] = json!(directory);
            if value["configured"] == true {
                value["settings"] = serde_json::to_value(ServiceConfig::load(
                    &directory.join("telegram-service.json"),
                )?)
                .map_err(|_| "Cannot encode Telegram settings")?;
            }
            Ok(Reply::Data(value))
        }
        Request::Configure { expected, config } => {
            let _lock = telegram_config::configuration_lock(directory)?;
            let path = directory.join("telegram-service.json");
            let current = telegram_config::read_private(&path, 1024 * 1024)?
                .map(|_| ServiceConfig::load(&path))
                .transpose()?;
            if serde_json::to_value(current).map_err(|_| "Invalid Telegram configuration")?
                != serde_json::to_value(expected).map_err(|_| "Invalid Telegram configuration")?
            {
                return Err("Telegram configuration changed; reopen settings".into());
            }
            Ok(Reply::Plan(Box::new(Plan::Configuration(
                telegram_config::ServicePlan::prepare(&path, *config)?,
            ))))
        }
        Request::Token { value } => Ok(Reply::Plan(Box::new(Plan::Token(TokenPlan::prepare(
            directory, &value,
        )?)))),
        Request::Service { start } => {
            let configuration = telegram_config::read_private(
                &directory.join("telegram-service.json"),
                1024 * 1024,
            )?
            .ok_or("Configure Telegram first")?;
            let config = ServiceConfig::load(&directory.join("telegram-service.json"))?;
            let status = super::telegram_supervisor::inspect(directory)?;
            if start {
                config.read_token()?;
                config.check_registrations()?;
                super::cutover::activation_check(directory)?;
                if status.state != "stopped" {
                    return Err("Service is not stopped; refresh status".into());
                }
            } else if status.process.is_none() {
                return Err("No verified service process to stop".into());
            }
            Ok(Reply::Plan(Box::new(Plan::Service(ServicePlan {
                directory: directory.into(),
                configuration,
                start,
                process: status.process,
            }))))
        }
        Request::Apply { plan, approval } => {
            if plan.approval() != approval {
                return Err("Telegram approval does not match the reviewed plan".into());
            }
            let result = match *plan {
                Plan::Route(plan) => plan.apply(directory, &live_terminals(directory)?)?,
                Plan::Binding(plan) => plan.apply(directory, &live_terminals(directory)?)?,
                Plan::Configuration(plan) => {
                    json!({"state":"saved","changed":plan.apply(&approval)?,"service_restart_may_be_required":true})
                }
                Plan::Token(plan) => {
                    if plan.directory != directory {
                        return Err("Token plan belongs to another instance".into());
                    }
                    plan.apply(&approval)?
                }
                Plan::Service(plan) => {
                    if plan.directory != directory {
                        return Err("Service plan belongs to another instance".into());
                    }
                    let _lock = telegram_config::configuration_lock(directory)?;
                    if telegram_config::read_private(
                        &directory.join("telegram-service.json"),
                        1024 * 1024,
                    )?
                    .as_ref()
                        != Some(&plan.configuration)
                    {
                        return Err("Telegram configuration changed; review again".into());
                    }
                    if plan.start {
                        super::telegram_supervisor::start(directory)?
                    } else {
                        super::telegram_supervisor::stop(
                            directory,
                            plan.process.as_ref().ok_or("Missing service process")?,
                        )?
                    }
                }
            };
            Ok(Reply::Data(result))
        }
    }
}

/// Display metadata only. Chat titles never grant access or resolve a terminal.
fn chat_names(directory: &Path, bot_id: i64) -> Result<std::collections::BTreeMap<i64, String>> {
    let Some(bytes) = layouts::read_private(&directory.join("telegram-chats.json"))? else {
        return Ok(Default::default());
    };
    let value: Value =
        serde_json::from_slice(&bytes).map_err(|_| "Invalid private chat catalog")?;
    if value["bot_id"].as_i64() != Some(bot_id) {
        return Ok(Default::default());
    }
    let chats = value["chats"]
        .as_array()
        .ok_or("Invalid private chat catalog")?;
    if chats.len() > 1000 {
        return Err("Chat catalog exceeds limit".into());
    }
    Ok(chats
        .iter()
        .filter_map(|chat| {
            let id = chat["chat"].as_i64().filter(|id| *id != 0)?;
            let title: String = chat["title"]
                .as_str()?
                .chars()
                .filter(|c| !c.is_control())
                .take(120)
                .collect();
            Some((id, title))
        })
        .collect())
}

/// Read-only pairing inventory. A local operator must still review and apply a
/// grant; an incoming update never grants itself access or advances the cursor.
fn discover_chats(config: &ServiceConfig, api: &mut impl Api) -> Result<Value> {
    let bot = api
        .get_me()
        .map_err(|_| "Cannot verify Telegram bot identity")?;
    if bot["id"].as_i64() != Some(config.bot_id) || bot["is_bot"] != true {
        return Err("Telegram token belongs to another bot".into());
    }
    let username = bot["username"].as_str().unwrap_or("");
    let updates = api
        .updates(0, 0)
        .map_err(|_| "Cannot discover Telegram chats; send a new /start to the bot and retry")?;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|_| "Invalid local clock")?
        .as_secs() as i64;
    Ok(discovery_candidates(config.bot_id, username, updates, now))
}

fn discovery_candidates(bot_id: i64, username: &str, updates: Vec<Value>, now: i64) -> Value {
    let mut found = std::collections::BTreeMap::<(i64, i64), Value>::new();
    for update in updates {
        let message = &update["message"];
        let Some(chat) = message["chat"]["id"].as_i64() else {
            continue;
        };
        let Some(actor) = message["from"]["id"].as_i64() else {
            continue;
        };
        let kind = message["chat"]["type"].as_str().unwrap_or("");
        if !message["date"]
            .as_i64()
            .is_some_and(|date| date >= now - 600 && date <= now + 30)
        {
            continue;
        }
        if actor <= 0 || message["from"]["is_bot"] != false || message.get("sender_chat").is_some()
        {
            continue;
        }
        if !((kind == "private" && chat == actor)
            || (matches!(kind, "group" | "supergroup") && chat < 0))
        {
            continue;
        }
        let command = message["text"]
            .as_str()
            .unwrap_or("")
            .split_whitespace()
            .next()
            .unwrap_or("");
        let addressed = command == "/start" && kind == "private"
            || !username.is_empty()
                && (command.eq_ignore_ascii_case(&format!("/start@{username}"))
                    || command.eq_ignore_ascii_case(&format!("/connect@{username}")));
        if !addressed {
            continue;
        }
        found.insert(
            (chat, actor),
            json!({"chat":chat,"actor":actor,"kind":kind,
                "title":message["chat"]["title"].as_str().unwrap_or("Direct message")
                    .chars().filter(|c| !c.is_control()).take(120).collect::<String>()}),
        );
    }
    json!({"bot_id":bot_id,"bot_username":username,"chats":found.into_values().collect::<Vec<_>>(),"note":"Read-only candidates. Select one and review its sender grant locally; no route or delivery is created automatically."})
}

fn live_terminals(directory: &Path) -> Result<Vec<crate::core::routes::Terminal>> {
    use super::telegram_commands::Operations;
    let config = super::config::ControlConfig::parse(
        &layouts::read_private(&directory.join("control.json"))?.ok_or("Run setup first")?,
    )?;
    let instance = ServiceConfig::load(&directory.join("telegram-service.json"))?.instance;
    super::telegram_commands::NativeOperations {
        instance,
        directory: directory.into(),
        config,
        terminals: crate::adapters::terminals::Terminals::default(),
        environment: crate::platform::environment(),
    }
    .inventory()
}
fn route_bindings(
    directory: &Path,
    chat: i64,
    actor: i64,
    live: &[crate::core::routes::Terminal],
) -> Result<Value> {
    let hooks = directory.join("hooks");
    layouts::check_path(&hooks)?;
    let mut recipients = Vec::new();
    match std::fs::read_dir(&hooks) {
        Ok(entries) => {
            for (index, entry) in entries.enumerate() {
                if index >= 1024 {
                    return Err("Hook inventory exceeds limit".into());
                }
                let path = entry.map_err(|_| "Cannot inspect hook inventory")?.path();
                if path.extension().and_then(|v| v.to_str()) != Some("json") {
                    continue;
                }
                if let Ok(plan) = BindingPlan::prepare(directory, &path, chat, actor, live) {
                    let mut review = plan.review();
                    review["registration_path"] = json!(path);
                    recipients.push(review);
                }
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => (),
        Err(_) => return Err("Cannot inspect installed hooks".into()),
    }
    recipients.sort_by_key(|v| v["registration_path"].as_str().unwrap_or("").to_owned());
    Ok(json!({"recipients":recipients}))
}

pub struct RoutePlan {
    directory: PathBuf,
    configuration: Vec<u8>,
    scope: crate::core::routes::Scope,
    actor: String,
    target: crate::core::routes::Terminal,
    previous: Option<crate::core::routes::Route>,
}
impl RoutePlan {
    pub(crate) fn prepare(
        directory: &Path,
        chat: i64,
        actor: i64,
        target: crate::core::routes::Terminal,
        live: &[crate::core::routes::Terminal],
    ) -> Result<Self> {
        let _lock = telegram_config::configuration_lock(directory)?;
        let configuration =
            telegram_config::read_private(&directory.join("telegram-service.json"), 1024 * 1024)?
                .ok_or("Configure Telegram first")?;
        let config = ServiceConfig::load(&directory.join("telegram-service.json"))?;
        if !config.grants.iter().any(|g| {
            g.chat == chat
                && g.actor == actor
                && g.permissions.contains(&telegram_config::Access::Manage)
        }) {
            return Err("Choose an explicitly authorized chat and route manager".into());
        }
        if !target.available
            || live
                .iter()
                .filter(|t| t.identity == target.identity)
                .count()
                != 1
            || !live.contains(&target)
        {
            return Err("Selected terminal changed; refresh before routing".into());
        }
        let scope = crate::core::routes::Scope {
            instance: config.instance,
            bot: config.bot_id.to_string(),
            chat: chat.to_string(),
        };
        let previous =
            super::diagnostics::with_database(&directory.join("routes.sqlite3"), |con| {
                crate::core::routes::read_route(con, &scope).map_err(|e| e.to_string())
            })?
            .flatten();
        Ok(Self {
            directory: directory.into(),
            configuration,
            scope,
            actor: actor.to_string(),
            target,
            previous,
        })
    }
    fn approval(&self) -> String {
        format!(
            "{:x}",
            Sha256::digest(
                serde_json::to_vec(&(
                    &self.directory,
                    &self.configuration,
                    &self.scope,
                    &self.actor,
                    &self.target,
                    &self.previous
                ))
                .expect("serializable route plan")
            )
        )
    }
    fn review(&self) -> Value {
        json!({"operation":"route", "chat":self.scope.chat,"actor":self.actor,"terminal":self.target,"previous":self.previous,"approval":self.approval(),"effect":"Future messages use this exact terminal; existing queued messages retain their original route."})
    }
    fn apply(self, directory: &Path, live: &[crate::core::routes::Terminal]) -> Result<Value> {
        if self.directory != directory {
            return Err("Route plan belongs to another instance".into());
        }
        let _lock = telegram_config::configuration_lock(directory)?;
        super::cutover::activation_check(directory)?;
        if telegram_config::read_private(&directory.join("telegram-service.json"), 1024 * 1024)?
            .as_ref()
            != Some(&self.configuration)
        {
            return Err("Telegram permissions changed; review route again".into());
        }
        if live
            .iter()
            .filter(|t| t.identity == self.target.identity)
            .count()
            != 1
            || !live.contains(&self.target)
        {
            return Err("Selected terminal changed; refresh before routing".into());
        }
        let config = ServiceConfig::load(&directory.join("telegram-service.json"))?;
        let mut registry = crate::core::routes::Registry::from_connection(
            telegram_config::private_connection(&directory.join("routes.sqlite3"))?,
        )
        .map_err(|e| e.to_string())?;
        registry
            .replace_grants(
                &config.instance,
                &config.bot_id.to_string(),
                &config
                    .grants
                    .iter()
                    .map(|g| (g.chat.to_string(), g.actor.to_string(), g.permissions()))
                    .collect::<Vec<_>>(),
            )
            .map_err(|e| e.to_string())?;
        let endpoint = live
            .iter()
            .filter(|t| {
                t.identity.backend == self.target.identity.backend
                    && t.identity.endpoint == self.target.identity.endpoint
            })
            .cloned()
            .collect::<Vec<_>>();
        registry
            .inventory(
                self.target.identity.backend.clone(),
                &self.target.identity.endpoint,
                &endpoint,
                true,
                true,
            )
            .map_err(|e| e.to_string())?;
        let route = registry
            .select(
                &self.scope,
                &self.actor,
                &self.target.identity,
                &live.iter().map(|t| t.identity.clone()).collect::<Vec<_>>(),
                self.previous.as_ref().map_or(0, |r| r.revision),
                true,
                true,
            )
            .map_err(|e| e.to_string())?;
        Ok(json!({"state":"saved","route":route}))
    }
}

/// Consent for a single installed hook registration and a selected recipient.
/// Frozen bytes and route revision are rechecked while holding the same config
/// lock used by local route updates; hook delivery rechecks them again at send.
pub struct BindingPlan {
    directory: PathBuf,
    registration_path: PathBuf,
    registration_bytes: Vec<u8>,
    configuration: Vec<u8>,
    binding: super::hook_effects::Binding,
    after: ServiceConfig,
}
impl BindingPlan {
    fn prepare(
        directory: &Path,
        path: &Path,
        chat: i64,
        actor: i64,
        live: &[crate::core::routes::Terminal],
    ) -> Result<Self> {
        let _lock = telegram_config::configuration_lock(directory)?;
        let configuration =
            telegram_config::read_private(&directory.join("telegram-service.json"), 1024 * 1024)?
                .ok_or("Configure Telegram first")?;
        let mut config: ServiceConfig =
            serde_json::from_slice(&configuration).map_err(|_| "Invalid Telegram configuration")?;
        config.validate()?;
        let registration_bytes = telegram_config::read_private(path, 1024 * 1024)?
            .ok_or("Installed hook registration is missing")?;
        let registration: super::hook_runtime::Registration =
            serde_json::from_slice(&registration_bytes).map_err(|_| "Invalid hook registration")?;
        registration.validate().map_err(|e| e.to_string())?;
        if registration.directory != directory
            || registration.path() != path
            || registration.scope.instance != config.instance
        {
            return Err("Hook registration belongs to another instance".into());
        }
        if !config.grants.iter().any(|g| {
            g.chat == chat
                && g.actor == actor
                && g.permissions.contains(&telegram_config::Access::Send)
                && g.permissions.contains(&telegram_config::Access::Read)
        }) {
            return Err("Select an explicitly authorized recipient".into());
        }
        let matching = live
            .iter()
            .filter(|t| t.identity == registration.scope.terminal)
            .collect::<Vec<_>>();
        if matching.len() != 1
            || !matching[0].available
            || matching[0].conversation.as_deref() != Some(registration.scope.conversation.as_str())
            || serde_json::to_value(registration.scope.harness)
                .ok()
                .as_ref()
                != Some(&json!(matching[0].harness))
        {
            return Err("Hook conversation changed; refresh the terminal".into());
        }
        let scope = crate::core::routes::Scope {
            instance: config.instance.clone(),
            bot: config.bot_id.to_string(),
            chat: chat.to_string(),
        };
        let route = super::diagnostics::with_database(&directory.join("routes.sqlite3"), |con| {
            crate::core::routes::read_route(con, &scope).map_err(|e| e.to_string())
        })?
        .flatten()
        .ok_or("Select this terminal as the chat route first")?;
        if route.terminal != registration.scope.terminal || route.revision <= 0 {
            return Err("Recipient route targets another terminal".into());
        }
        let binding = super::hook_effects::Binding {
            registration_id: registration.id,
            goal_scope: registration.scope,
            route_scope: scope,
            actor: actor.to_string(),
            route_revision: route.revision,
            reply_to: None,
            hindsight_bank: None,
        };
        if !config.registrations.contains(&path.to_path_buf()) {
            config.registrations.push(path.into());
        }
        // Rebinding explicitly replaces only this registration/recipient pair.
        config.hook_bindings.retain(|b| {
            !(b.registration_id == binding.registration_id
                && b.route_scope == binding.route_scope
                && b.actor == binding.actor)
        });
        config.hook_bindings.push(binding.clone());
        config.validate()?;
        Ok(Self {
            directory: directory.into(),
            registration_path: path.into(),
            registration_bytes,
            configuration,
            binding,
            after: config,
        })
    }
    fn approval(&self) -> String {
        format!(
            "{:x}",
            Sha256::digest(
                serde_json::to_vec(&(
                    &self.directory,
                    &self.registration_path,
                    &self.registration_bytes,
                    &self.configuration,
                    &self.binding
                ))
                .expect("binding plan")
            )
        )
    }
    fn review(&self) -> Value {
        json!({"operation":"hook_binding","registration":self.binding.registration_id,"chat":self.binding.route_scope.chat,"actor":self.binding.actor,"conversation":self.binding.goal_scope.conversation,"route_revision":self.binding.route_revision,"approval":self.approval(),"effect":"Enable hook delivery to this explicitly selected recipient. Reload Telegram settings to load the updated recipient consent."})
    }
    fn apply(self, directory: &Path, live: &[crate::core::routes::Terminal]) -> Result<Value> {
        if directory != self.directory {
            return Err("Hook binding belongs to another instance".into());
        }
        // Preparing again checks current consent, exact live conversation and route.
        let fresh = Self::prepare(
            directory,
            &self.registration_path,
            self.binding
                .route_scope
                .chat
                .parse()
                .map_err(|_| "Invalid chat")?,
            self.binding.actor.parse().map_err(|_| "Invalid actor")?,
            live,
        )?;
        if fresh.approval() != self.approval() {
            return Err("Hook registration, route or permissions changed; review again".into());
        }
        let _lock = telegram_config::configuration_lock(directory)?;
        super::cutover::activation_check(directory)?;
        if telegram_config::read_private(&directory.join("telegram-service.json"), 1024 * 1024)?
            .as_ref()
            != Some(&self.configuration)
            || telegram_config::read_private(&self.registration_path, 1024 * 1024)?.as_ref()
                != Some(&self.registration_bytes)
        {
            return Err("Hook configuration changed; review again".into());
        }
        let route = super::diagnostics::with_database(&directory.join("routes.sqlite3"), |con| {
            crate::core::routes::read_route(con, &self.binding.route_scope)
                .map_err(|e| e.to_string())
        })?
        .flatten()
        .ok_or("Hook route is missing")?;
        if route.terminal != self.binding.goal_scope.terminal
            || route.revision != self.binding.route_revision
        {
            return Err("Hook route changed; review again".into());
        }
        let registration: super::hook_runtime::Registration =
            serde_json::from_slice(&self.registration_bytes).map_err(|_| "Invalid registration")?;
        if let Some(process) = &registration.harness_process {
            super::hook_runtime::verify_process(process, Path::new("/proc"))
                .map_err(|_| "Hook process changed; reinstall hooks")?;
        }
        let backup = directory.join(format!(
            "telegram-service.backup-{:x}.json",
            Sha256::digest(&self.configuration)
        ));
        match telegram_config::read_private(&backup, 1024 * 1024)? {
            Some(bytes) if bytes != self.configuration => {
                return Err("Configuration backup conflicts".into());
            }
            Some(_) => (),
            None => layouts::write_private(&backup, &self.configuration)?,
        }
        layouts::write_private(
            &directory.join("telegram-service.json"),
            &serde_json::to_vec_pretty(&self.after).map_err(|_| "Cannot encode hook binding")?,
        )?;
        Ok(
            json!({"state":"saved","service_restart_required":false,"policy_reload":"next_service_check"}),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{collections::BTreeSet, fs};
    #[test]
    fn pairing_candidates_require_addressed_human_dm_or_group_message() {
        let message = |id, chat, actor, kind, text| json!({"update_id":id,"message":{"chat":{"id":chat,"type":kind},"from":{"id":actor,"is_bot":false},"text":text,"date":1000}});
        let result = discovery_candidates(
            123,
            "FixtureBot",
            vec![
                message(1, 42, 42, "private", "/start"),
                message(2, -100, 42, "supergroup", "/start@FixtureBot"),
                message(3, -101, 42, "group", "ordinary text"),
                message(4, -102, 42, "group", "/start@OtherBot"),
                message(5, 43, 42, "private", "/start"),
                json!({"update_id":6,"message":{"chat":{"id":-103,"type":"group"},"from":{"id":44,"is_bot":true},"text":"/start@FixtureBot","date":1000}}),
                json!({"update_id":7,"message":{"chat":{"id":-104,"type":"group"},"from":{"id":44,"is_bot":false},"text":"/start@FixtureBot","date":200}}),
            ],
            1000,
        );
        let rows = result["chats"].as_array().unwrap();
        assert_eq!(rows.len(), 2);
        assert!(rows.iter().any(|v| v["chat"] == 42 && v["actor"] == 42));
        assert!(rows.iter().any(|v| v["chat"] == -100 && v["actor"] == 42));
        assert!(!result.to_string().contains("ordinary text"));
    }
    struct Fixture(PathBuf);
    impl Fixture {
        fn new() -> Self {
            let root = std::env::temp_dir().join(format!(
                "daedalus-token-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
            let config = ServiceConfig {
                version: 1,
                instance: "fixture".into(),
                bot_id: 123,
                token_file: root.join("token"),
                grants: vec![],
                listen_chats: BTreeSet::new(),
                registrations: vec![],
                hook_bindings: vec![],
            };
            layouts::write_private(
                &root.join("telegram-service.json"),
                &serde_json::to_vec(&config).unwrap(),
            )
            .unwrap();
            Self(root)
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
    #[test]
    fn preview_redacts_and_apply_saves_private_raw_token() {
        let f = Fixture::new();
        let plan = TokenPlan::prepare(&f.0, "123:fixture_secret").unwrap();
        assert!(!plan.review().to_string().contains("fixture_secret"));
        assert!(!f.0.join("token").exists());
        let approval = plan.approval();
        assert_eq!(plan.apply(&approval).unwrap()["state"], "saved");
        let config = ServiceConfig::load(&f.0.join("telegram-service.json")).unwrap();
        assert_eq!(config.read_token().unwrap(), "123:fixture_secret");
    }
    #[test]
    fn changed_token_and_wrong_identity_are_rejected() {
        let f = Fixture::new();
        assert!(TokenPlan::prepare(&f.0, "999:fixture_secret").is_err());
        let plan = TokenPlan::prepare(&f.0, "123:fixture_secret").unwrap();
        let approval = plan.approval();
        layouts::write_private(&f.0.join("token"), b"123:newer_secret").unwrap();
        assert!(plan.apply(&approval).is_err());
        assert_eq!(fs::read(f.0.join("token")).unwrap(), b"123:newer_secret");
    }
    #[test]
    fn replacement_backs_up_previous_token_and_import_can_be_configured() {
        let f = Fixture::new();
        layouts::write_private(
            &f.0.join("migration.json"),
            b"{\"activation\":\"requires_cutover\"}",
        )
        .unwrap();
        layouts::write_private(&f.0.join("token"), b"123:old_secret").unwrap();
        let plan = TokenPlan::prepare(&f.0, "123:replacement_secret").unwrap();
        let approval = plan.approval();
        plan.apply(&approval).unwrap();
        let backup = f.0.join(format!(
            "telegram-token.backup-{:x}",
            Sha256::digest(b"123:old_secret")
        ));
        assert_eq!(fs::read(backup).unwrap(), b"123:old_secret");
    }
    fn route_fixture() -> (Fixture, crate::core::routes::Terminal) {
        let fixture = Fixture::new();
        let path = fixture.0.join("telegram-service.json");
        let mut config = ServiceConfig::load(&path).unwrap();
        config.grants.push(telegram_config::Grant {
            chat: 42,
            actor: 7,
            permissions: vec![
                telegram_config::Access::Read,
                telegram_config::Access::Send,
                telegram_config::Access::Manage,
            ],
        });
        layouts::write_private(&path, &serde_json::to_vec(&config).unwrap()).unwrap();
        let terminal = crate::core::routes::Terminal {
            identity: crate::core::identity::TerminalId {
                backend: crate::core::identity::Backend::Tmux,
                endpoint: crate::test_support::path("/fixture.sock").into(),
                pane: "%1".into(),
                generation: "generation".into(),
            },
            label: "Workspace".into(),
            workspace: crate::test_support::path("/workspace").into(),
            harness: "claude".into(),
            conversation: Some("conversation".into()),
            available: true,
        };
        (fixture, terminal)
    }
    #[test]
    fn chat_titles_are_private_display_metadata_bound_to_the_bot() {
        let f = Fixture::new();
        layouts::write_private(
            &f.0.join("telegram-chats.json"),
            &serde_json::to_vec(
                &json!({"bot_id":123,"chats":[{"chat":-100,"title":"Team\n\u{1b}"}]}),
            )
            .unwrap(),
        )
        .unwrap();
        assert_eq!(chat_names(&f.0, 123).unwrap().get(&-100).unwrap(), "Team");
        assert!(chat_names(&f.0, 456).unwrap().is_empty());
        assert!(
            ServiceConfig::load(&f.0.join("telegram-service.json"))
                .unwrap()
                .grants
                .is_empty()
        );
    }
    #[test]
    fn route_preview_is_inert_and_concurrent_selection_cannot_overwrite() {
        let (f, target) = route_fixture();
        let live = vec![target.clone()];
        let first = RoutePlan::prepare(&f.0, 42, 7, target.clone(), &live).unwrap();
        let stale = RoutePlan::prepare(&f.0, 42, 7, target, &live).unwrap();
        assert!(!f.0.join("routes.sqlite3").exists());
        let result = first.apply(&f.0, &live).unwrap();
        assert_eq!(result["route"]["revision"], 1);
        assert_eq!(result["route"]["pinned"], true);
        assert!(
            stale
                .apply(&f.0, &live)
                .unwrap_err()
                .contains("Route changed")
        );
    }
    #[test]
    fn route_rejects_changed_conversation_and_revoked_grant() {
        let (f, target) = route_fixture();
        let live = vec![target.clone()];
        assert!(RoutePlan::prepare(&f.0, 42, 999, target.clone(), &live).is_err());
        let plan = RoutePlan::prepare(&f.0, 42, 7, target.clone(), &live).unwrap();
        let mut changed = target.clone();
        changed.conversation = Some("other".into());
        assert!(
            plan.apply(&f.0, &[changed])
                .unwrap_err()
                .contains("terminal changed")
        );
        assert!(!f.0.join("routes.sqlite3").exists());
        let plan = RoutePlan::prepare(&f.0, 42, 7, target, &live).unwrap();
        let path = f.0.join("telegram-service.json");
        let mut config = ServiceConfig::load(&path).unwrap();
        config.grants.clear();
        layouts::write_private(&path, &serde_json::to_vec(&config).unwrap()).unwrap();
        assert!(
            plan.apply(&f.0, &live)
                .unwrap_err()
                .contains("permissions changed")
        );
        assert!(!f.0.join("routes.sqlite3").exists());
    }
    fn binding_fixture() -> (Fixture, crate::core::routes::Terminal, PathBuf) {
        let (f, target) = route_fixture();
        RoutePlan::prepare(&f.0, 42, 7, target.clone(), std::slice::from_ref(&target))
            .unwrap()
            .apply(&f.0, std::slice::from_ref(&target))
            .unwrap();
        let registration = super::super::hook_runtime::Registration {
            version: 1,
            id: "fixture-hook".into(),
            directory: f.0.clone(),
            scope: crate::core::goal_store::Scope {
                instance: "fixture".into(),
                terminal: target.identity.clone(),
                harness: super::super::hooks::Harness::Claude,
                conversation: "conversation".into(),
            },
            goal_id: None,
            harness_process: None,
            server_emitter: None,
            transcript: None,
            events: vec![super::super::hooks::EventKind::Stop],
            options: super::super::hook_runtime::Options::default(),
            activity_lease_ms: 1000,
            pending_timeout_ms: 1000,
            hook_timeout_ms: 1000,
        };
        let path = registration.path();
        layouts::write_private(&path, &serde_json::to_vec(&registration).unwrap()).unwrap();
        (f, target, path)
    }
    #[test]
    fn rerouting_offers_only_exact_installed_conversation_and_explicit_recipient() {
        let (f, target, path) = binding_fixture();
        let live = vec![target.clone()];
        let choices = route_bindings(&f.0, 42, 7, &live).unwrap();
        assert_eq!(choices["recipients"].as_array().unwrap().len(), 1);
        assert_eq!(choices["recipients"][0]["registration_path"], json!(path));
        assert!(
            ServiceConfig::load(&f.0.join("telegram-service.json"))
                .unwrap()
                .hook_bindings
                .is_empty()
        );
        assert!(
            route_bindings(&f.0, 42, 999, &live).unwrap()["recipients"]
                .as_array()
                .unwrap()
                .is_empty()
        );
        let mut changed = target;
        changed.conversation = Some("other-conversation".into());
        assert!(
            route_bindings(&f.0, 42, 7, &[changed]).unwrap()["recipients"]
                .as_array()
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn binding_requires_explicit_recipient_and_preserves_settings() {
        let (f, target, path) = binding_fixture();
        let live = vec![target];
        assert!(BindingPlan::prepare(&f.0, &path, 42, 999, &live).is_err());
        let configuration = f.0.join("telegram-service.json");
        let original = fs::read(&configuration).unwrap();
        let mut send_only = ServiceConfig::load(&configuration).unwrap();
        send_only.grants[0].permissions = vec![telegram_config::Access::Send];
        layouts::write_private(&configuration, &serde_json::to_vec(&send_only).unwrap()).unwrap();
        assert!(BindingPlan::prepare(&f.0, &path, 42, 7, &live).is_err());
        layouts::write_private(&configuration, &original).unwrap();
        let before = fs::read(f.0.join("telegram-service.json")).unwrap();
        let plan = BindingPlan::prepare(&f.0, &path, 42, 7, &live).unwrap();
        assert_eq!(fs::read(f.0.join("telegram-service.json")).unwrap(), before);
        assert_eq!(plan.review()["actor"], "7");
        plan.apply(&f.0, &live).unwrap();
        let config = ServiceConfig::load(&f.0.join("telegram-service.json")).unwrap();
        assert_eq!(config.registrations, vec![path.clone()]);
        assert_eq!(config.hook_bindings.len(), 1);
        assert_eq!(config.hook_bindings[0].route_revision, 1);
        assert_eq!(config.grants.len(), 1);
        BindingPlan::prepare(&f.0, &path, 42, 7, &live)
            .unwrap()
            .apply(&f.0, &live)
            .unwrap();
        assert_eq!(
            ServiceConfig::load(&f.0.join("telegram-service.json"))
                .unwrap()
                .hook_bindings
                .len(),
            1
        );
    }
    #[test]
    fn binding_rejects_replaced_conversation_registration_and_route() {
        let (f, target, path) = binding_fixture();
        let live = vec![target.clone()];
        let plan = BindingPlan::prepare(&f.0, &path, 42, 7, &live).unwrap();
        let mut replacement = target.clone();
        replacement.conversation = Some("replacement".into());
        assert!(plan.apply(&f.0, &[replacement]).is_err());
        let plan = BindingPlan::prepare(&f.0, &path, 42, 7, &live).unwrap();
        let before = fs::read(&path).unwrap();
        let mut altered = before.clone();
        altered.push(b' ');
        layouts::write_private(&path, &altered).unwrap();
        assert!(plan.apply(&f.0, &live).is_err());
        layouts::write_private(&path, &before).unwrap();
        let plan = BindingPlan::prepare(&f.0, &path, 42, 7, &live).unwrap();
        RoutePlan::prepare(&f.0, 42, 7, target, &live)
            .unwrap()
            .apply(&f.0, &live)
            .unwrap();
        assert!(plan.apply(&f.0, &live).is_err());
        assert!(
            ServiceConfig::load(&f.0.join("telegram-service.json"))
                .unwrap()
                .hook_bindings
                .is_empty()
        );
    }
}
