//! Durable Telegram admission and feeder dispatch. One owner per instance.
//! Authenticated Bot API updates are data; sender-controlled names are never IDs.
use crate::{
    adapters::telegram::{Api, Error as TransportError, FILE_LIMIT},
    core::{
        identity::TerminalId,
        journal::{Delivery, Journal},
        layouts,
        routes::{Permission, Registry, Scope},
    },
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::PathBuf,
};
pub type Result<T> = std::result::Result<T, String>;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Attachment {
    pub path: PathBuf,
    pub sha256: String,
    pub bytes: usize,
    pub name: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Frame {
    pub scope: Scope,
    pub actor: String,
    pub message_id: i64,
    pub update_id: i64,
    pub terminal: TerminalId,
    pub route_revision: i64,
    pub conversation: Option<String>,
    pub text: String,
    pub attachment: Option<Attachment>,
}
#[derive(Clone, Debug, PartialEq)]
pub enum Submission {
    Accepted(Value),
    Busy,
    Unavailable,
    Unknown,
}
pub enum Outbound {
    Ready(Box<Delivery>),
    Blocked,
    Empty,
}
/// Implementations must check exact liveness, harness identity and readiness
/// again at submission. A terminal write is not proof the model processed input.
pub trait Feeder {
    fn live(&mut self) -> Result<Vec<TerminalId>>;
    fn submit(&mut self, frame: &Frame) -> Result<Submission>;
}
pub trait Files {
    fn save(&mut self, name: &str, bytes: &[u8]) -> Result<Attachment>;
}
pub struct PrivateFiles {
    pub directory: PathBuf,
}
impl Files for PrivateFiles {
    fn save(&mut self, name: &str, bytes: &[u8]) -> Result<Attachment> {
        if bytes.len() > FILE_LIMIT {
            return Err("Attachment exceeds 20 MiB".into());
        }
        layouts::check_path(&self.directory)?;
        let sha = format!("{:x}", Sha256::digest(bytes));
        // Keep a short alphanumeric extension so agents can open images and
        // documents by type; the name itself never becomes part of the path.
        let extension = name
            .rsplit_once('.')
            .map(|(_, ext)| ext.to_ascii_lowercase())
            .filter(|ext| {
                (1..=10).contains(&ext.len()) && ext.bytes().all(|b| b.is_ascii_alphanumeric())
            })
            .unwrap_or_else(|| "bin".into());
        let path = self.directory.join(format!("{sha}.{extension}"));
        #[cfg(unix)]
        {
            use std::os::unix::fs::{DirBuilderExt, MetadataExt};
            std::fs::DirBuilder::new()
                .recursive(true)
                .mode(0o700)
                .create(&self.directory)
                .map_err(|_| "Cannot create private attachment directory")?;
            let meta = std::fs::metadata(&self.directory)
                .map_err(|_| "Cannot inspect attachment directory")?;
            if meta.uid() != unsafe { libc::geteuid() } || meta.mode() & 0o077 != 0 {
                return Err("Attachment directory must be private and owned by this user".into());
            }
        }
        #[cfg(windows)]
        crate::platform::ensure_private_dir(&self.directory)
            .map_err(|_| "Cannot create private owner-scoped attachment directory")?;
        if let Some(existing) = layouts::read_private_limit(&path, FILE_LIMIT as u64)? {
            if existing != bytes {
                return Err("Attachment content conflicts with existing file".into());
            }
        } else {
            layouts::write_private_limit(&path, bytes, FILE_LIMIT as u64)?;
        }
        let name: String = name
            .chars()
            .filter(|c| !c.is_control() && !"/\\".contains(*c))
            .take(255)
            .collect();
        Ok(Attachment {
            path,
            sha256: sha,
            bytes: bytes.len(),
            name,
        })
    }
}

pub struct Engine {
    pub journal: Journal,
    pub streams: Option<super::telegram_stream::Store>,
    pub routes: Registry,
    instance: String,
    bot: i64,
    username: String,
    verified: bool,
    stage_after: i64,
    controls_after: i64,
    controls_enabled: bool,
    hook_registrations: Vec<super::hook_runtime::Registration>,
    hook_bindings: Vec<Value>,
    pub listen_chats: BTreeSet<i64>,
    pub(crate) feedback_acks: Vec<super::telegram_feedback::Receipt>,
}
#[derive(Clone, Debug)]
struct Message {
    chat: i64,
    actor: i64,
    id: i64,
    text: String,
    file: Option<(String, String)>,
    human: bool,
    forwarded: bool,
}
impl Engine {
    pub fn new(journal: Journal, routes: Registry, instance: &str, bot: i64) -> Result<Self> {
        if bot <= 0 {
            return Err("Expected a positive Telegram bot ID".into());
        }
        Scope {
            instance: instance.into(),
            bot: bot.to_string(),
            chat: "0".into(),
        }
        .validate()
        .map_err(|e| e.to_string())?;
        Ok(Self {
            journal,
            streams: None,
            routes,
            instance: instance.into(),
            bot,
            username: String::new(),
            verified: false,
            stage_after: 0,
            controls_after: 0,
            controls_enabled: false,
            hook_registrations: vec![],
            hook_bindings: vec![],
            listen_chats: BTreeSet::new(),
            feedback_acks: vec![],
        })
    }
    /// Call once at service startup. Recovery never retries processing controls.
    pub fn start(&mut self, api: &mut impl Api) -> Result<()> {
        self.verified = false;
        let me = api.get_me().map_err(|e| e.to_string())?;
        self.start_verified(me)
    }
    /// Complete startup from a successful read-only getMe response. Transport
    /// retry policy belongs to the service; identity validation stays here.
    pub fn start_verified(&mut self, me: Value) -> Result<()> {
        self.verified = false;
        if me["id"].as_i64() != Some(self.bot) || me["is_bot"] != true {
            return Err("Telegram credential belongs to a different bot".into());
        }
        self.username = me["username"]
            .as_str()
            .filter(|v| !v.is_empty() && v.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_'))
            .ok_or("Telegram bot has no valid username")?
            .into();
        self.journal
            .recover_events("telegram", &self.bot.to_string())
            .map_err(|e| e.to_string())?;
        self.verified = true;
        Ok(())
    }
    fn ready(&self) -> Result<()> {
        if self.verified {
            Ok(())
        } else {
            Err("Verify Telegram bot identity before running the service".into())
        }
    }
    pub fn poll(&mut self, api: &mut impl Api, now: f64) -> Result<usize> {
        let offset = self.cursor()?;
        let updates = api.updates(offset, 50).map_err(|e| e.to_string())?;
        self.admit_updates(updates, now)
    }
    /// Only durable admission advances this offset. A poll worker must wait for
    /// the owner to admit its batch before requesting the next offset.
    pub fn cursor(&self) -> Result<i64> {
        self.ready()?;
        self.journal
            .cursor("telegram", &self.bot.to_string(), 0)
            .map_err(|e| e.to_string())
    }
    pub fn configure_hooks(
        &mut self,
        registrations: Vec<super::hook_runtime::Registration>,
        bindings: &[super::hook_effects::Binding],
    ) -> Result<()> {
        let mut ids = BTreeSet::new();
        for registration in &registrations {
            registration.validate().map_err(|e| e.to_string())?;
            if registration.scope.instance != self.instance || !ids.insert(&registration.id) {
                return Err("Hook policy contains ambiguous or foreign registrations".into());
            }
        }
        let mut reviewed = Vec::new();
        for binding in bindings {
            if binding.route_scope.instance != self.instance
                || binding.route_scope.bot != self.bot.to_string()
                || !registrations
                    .iter()
                    .any(|r| r.id == binding.registration_id && r.scope == binding.goal_scope)
            {
                return Err("Hook recipient is outside the configured service".into());
            }
            reviewed.push(serde_json::to_value(binding).map_err(|_| "Invalid hook recipient")?);
        }
        self.hook_registrations = registrations;
        self.hook_bindings = reviewed;
        Ok(())
    }
    pub fn admit_updates(&mut self, updates: Vec<Value>, now: f64) -> Result<usize> {
        self.ready()?;
        if updates.len() > 100 {
            return Err("Telegram update batch exceeds limit".into());
        }
        // A competing getUpdates can hand back ids this cursor already confirmed.
        // Re-admitting them replays old messages and must not move the cursor.
        let cursor = self.cursor()?;
        // Validate the entire batch before acknowledging any part of it.
        let mut ordered = BTreeMap::new();
        for update in updates {
            let id = update["update_id"]
                .as_i64()
                .filter(|id| *id >= 0 && *id < i64::MAX)
                .ok_or("Invalid Telegram update ID")?;
            if id < cursor {
                continue;
            }
            if serde_json::to_vec(&update)
                .map_err(|_| "Invalid Telegram update")?
                .len()
                > 1024 * 1024
            {
                return Err("Telegram update exceeds limit".into());
            }
            if let Some(previous) = ordered.insert(id, update.clone())
                && previous != update
            {
                return Err("Telegram update ID has conflicting payloads".into());
            }
        }
        let mut count = 0;
        for (id, update) in ordered {
            count += usize::from(
                self.journal
                    .admit_and_advance("telegram", &self.bot.to_string(), id, &update, now)
                    .map_err(|e| e.to_string())?,
            );
        }
        Ok(count)
    }
    fn scope(&self, chat: i64) -> Scope {
        Scope {
            instance: self.instance.clone(),
            bot: self.bot.to_string(),
            chat: chat.to_string(),
        }
    }
    fn parse(&self, update: &Value) -> Result<Option<Message>> {
        let Some(message) = update.get("message") else {
            return Ok(None);
        };
        // Anonymous/channel attribution cannot establish a user grant.
        if message.get("sender_chat").is_some() {
            return Ok(None);
        }
        let chat = message["chat"]["id"]
            .as_i64()
            .filter(|id| *id != 0)
            .ok_or("Invalid Telegram chat identity")?;
        let actor = message["from"]["id"]
            .as_i64()
            .filter(|id| *id > 0)
            .ok_or("Invalid Telegram actor identity")?;
        let id = message["message_id"]
            .as_i64()
            .filter(|id| *id > 0)
            .ok_or("Invalid Telegram message identity")?;
        let is_bot = message["from"]["is_bot"]
            .as_bool()
            .ok_or("Missing Telegram actor type")?;
        if actor == self.bot {
            return Ok(None);
        }
        let text = message["text"]
            .as_str()
            .or_else(|| message["caption"].as_str())
            .unwrap_or("");
        if text.len() > 60_000 || text.contains('\0') {
            return Err("Telegram input exceeds feeder limit".into());
        }
        let listen = self
            .routes
            .input_policy(&self.scope(chat))
            .map_err(|e| e.to_string())?
            .listen
            .unwrap_or_else(|| self.listen_chats.contains(&chat));
        if message["chat"]["type"] != "private" && !listen {
            let reply = message["reply_to_message"]["from"]["id"].as_i64() == Some(self.bot);
            let entities = message
                .get("entities")
                .or_else(|| message.get("caption_entities"))
                .and_then(Value::as_array);
            let mentioned = entities.is_some_and(|entities| {
                entities.iter().any(|e| {
                    let part = utf16_slice(text, e["offset"].as_u64(), e["length"].as_u64());
                    (e["type"] == "mention"
                        && part.as_ref().is_some_and(|s| {
                            s.eq_ignore_ascii_case(&format!("@{}", self.username))
                        }))
                        || (e["type"] == "bot_command"
                            && part.is_some_and(|s| {
                                s.split_once('@').is_some_and(|(_, name)| {
                                    name.eq_ignore_ascii_case(&self.username)
                                })
                            }))
                })
            });
            if !reply && !mentioned {
                return Ok(None);
            }
        }
        let file = if let Some(document) = message.get("document") {
            if document["file_size"]
                .as_u64()
                .is_some_and(|size| size > FILE_LIMIT as u64)
            {
                return Err("Telegram attachment exceeds 20 MiB".into());
            }
            Some((
                document["file_id"]
                    .as_str()
                    .filter(|s| !s.is_empty() && s.len() <= 1024)
                    .ok_or("Invalid Telegram attachment")?
                    .into(),
                document["file_name"]
                    .as_str()
                    .unwrap_or("attachment")
                    .into(),
            ))
        } else if let Some(photos) = message["photo"].as_array() {
            photos
                .iter()
                .filter(|p| {
                    p["file_size"]
                        .as_u64()
                        .is_none_or(|s| s <= FILE_LIMIT as u64)
                })
                .max_by_key(|p| {
                    p["width"]
                        .as_u64()
                        .unwrap_or(0)
                        .saturating_mul(p["height"].as_u64().unwrap_or(0))
                })
                .and_then(|p| p["file_id"].as_str())
                .map(|id| (id.into(), "photo.jpg".into()))
        } else {
            None
        };
        if text.is_empty() && file.is_none() {
            return Ok(None);
        } // No voice/transcription path.
        Ok(Some(Message {
            chat,
            actor,
            id,
            text: text.into(),
            file,
            human: !is_bot,
            forwarded: message.get("forward_origin").is_some(),
        }))
    }
    /// Dispatch controls before feeder staging. Persist intent before calling
    /// shared operations: a crash leaves an unknown control, never an auto-retry.
    pub fn stage_controls(
        &mut self,
        dispatcher: &mut super::telegram_commands::Dispatcher,
        operations: &mut impl super::telegram_commands::Operations,
        _api: &mut impl Api,
        now: f64,
    ) -> Result<usize> {
        use super::telegram_commands::{self as commands, Caller, Reply};
        self.ready()?;
        self.controls_enabled = true;
        if !now.is_finite() || now < 0.0 || now * 1000.0 > i64::MAX as f64 {
            return Err("Invalid control timestamp".into());
        }
        let events = self
            .journal
            .events_page(
                "telegram",
                &self.bot.to_string(),
                &["admitted"],
                self.controls_after,
                100,
            )
            .map_err(|e| e.to_string())?;
        if events.is_empty() {
            self.controls_after = 0;
            return Ok(0);
        }
        let mut handled = 0;
        for event in events {
            self.controls_after = event.id;
            let callback = event.payload.get("callback_query");
            let (message, callback_id, token) = if let Some(query) = callback {
                let query_message = &query["message"];
                let valid = query["from"]["is_bot"] == false
                    && query_message["from"]["id"].as_i64() == Some(self.bot)
                    && query_message["from"]["is_bot"] == true
                    && query.get("inline_message_id").is_none();
                let chat = query_message["chat"]["id"].as_i64().filter(|id| *id != 0);
                let actor = query["from"]["id"]
                    .as_i64()
                    .filter(|id| *id > 0 && *id != self.bot);
                let id = query_message["message_id"].as_i64().filter(|id| *id > 0);
                let callback_id = query["id"].as_str().filter(|v| {
                    !v.is_empty() && v.len() <= 256 && !v.chars().any(char::is_control)
                });
                let token = query["data"]
                    .as_str()
                    .filter(|v| !v.is_empty() && v.len() <= 64 && !v.chars().any(char::is_control));
                match (valid, chat, actor, id, callback_id, token) {
                    (true, Some(chat), Some(actor), Some(id), Some(callback_id), Some(token)) => (
                        Message {
                            chat,
                            actor,
                            id,
                            text: String::new(),
                            file: None,
                            human: true,
                            forwarded: query_message.get("forward_origin").is_some(),
                        },
                        Some(callback_id),
                        Some(token),
                    ),
                    _ => {
                        self.journal
                            .event_state(event.id, "ignored", Some("Invalid callback attribution"))
                            .map_err(|e| e.to_string())?;
                        continue;
                    }
                }
            } else {
                match self.parse(&event.payload) {
                    Ok(Some(message)) if message.text.starts_with('/') => (message, None, None),
                    _ => continue,
                }
            };
            let caller = Caller {
                scope: self.scope(message.chat),
                actor: message.actor.to_string(),
                human: message.human,
                forwarded: message.forwarded,
                bot_username: self.username.clone(),
                event_id: event.event_id.clone(),
            };
            if !caller.human
                || caller.forwarded
                || self
                    .routes
                    .authorize(&caller.scope, &caller.actor, Permission::Read)
                    .is_err()
            {
                self.journal
                    .event_state(event.id, "ignored", Some("Unauthorized control"))
                    .map_err(|e| e.to_string())?;
                continue;
            }
            let command = if token.is_none() {
                match commands::parse(&message.text, &self.username) {
                    Ok(Some(command)) => Some(command),
                    Ok(None) => {
                        self.journal
                            .event_state(event.id, "ignored", None)
                            .map_err(|e| e.to_string())?;
                        continue;
                    }
                    Err(_) => {
                        self.enqueue_text(
                            message.chat,
                            "Invalid command. Use /help for the supported syntax.",
                            Some(message.id),
                            &format!("update:{}:invalid", event.event_id),
                            now,
                        )?;
                        self.journal
                            .event_state(event.id, "failed", Some("Invalid control command"))
                            .map_err(|e| e.to_string())?;
                        continue;
                    }
                }
            } else {
                None
            };
            // Explicit prompts remain admitted for the normal feeder pipeline. No
            // side effect or cursor acknowledgement occurs in the control handler.
            if matches!(command, Some(commands::Command::Say(_))) {
                continue;
            }
            self.journal
                .event_state(event.id, "processing", None)
                .map_err(|e| e.to_string())?;
            let result = match (command, token) {
                (Some(command), None) => dispatcher.command(
                    &caller,
                    command,
                    &mut self.routes,
                    operations,
                    (now * 1000.0) as u64,
                ),
                (None, Some(token)) => dispatcher.callback(
                    &caller,
                    token,
                    &mut self.routes,
                    operations,
                    (now * 1000.0) as u64,
                ),
                _ => unreachable!(),
            };
            let success = result.is_ok();
            // Adapter errors never forward raw command output, paths or HTTP
            // request details to a chat. Inspect local diagnostics for evidence.
            let reply = result.unwrap_or_else(|_| Reply {text:"Control could not complete. Check permissions, refresh the target and open a new confirmation. An uncertain action will not be repeated automatically.".into(),buttons:vec![]});
            self.enqueue_control_reply(
                &caller,
                message.chat,
                Some(message.id),
                &reply,
                &format!("update:{}:control", event.event_id),
                now,
            )?;
            self.journal
                .event_state(
                    event.id,
                    if success { "handled" } else { "unknown" },
                    if success {
                        None
                    } else {
                        Some("Control failed or outcome unknown")
                    },
                )
                .map_err(|e| e.to_string())?;
            if let Some(id) = callback_id {
                // The outbound worker dismisses the client spinner. Slow HTTP
                // cannot block admission or cause a control to run again.
                self.journal.enqueue("outbound",&format!("telegram:{}:{}",self.bot,message.chat),
                    &json!({"chat":message.chat,"callback":id,"text":if success {"Recorded"} else {"Check the response"}}),
                    &format!("telegram:{}:update:{}:ack",self.bot,event.event_id),None,now).map_err(|e|e.to_string())?;
            }
            handled += 1;
        }
        Ok(handled)
    }
    fn enqueue_control_reply(
        &mut self,
        caller: &super::telegram_commands::Caller,
        chat: i64,
        reply_to: Option<i64>,
        reply: &super::telegram_commands::Reply,
        key: &str,
        now: f64,
    ) -> Result<()> {
        if reply.text.is_empty() || reply.text.len() > 256 * 1024 || reply.text.contains('\0') {
            return Err("Invalid control response".into());
        }
        let route = self
            .routes
            .inspect(&caller.scope, &caller.actor)
            .map_err(|e| e.to_string())?;
        let conversation = match &route {
            Some(route) => self
                .routes
                .terminal(&route.terminal)
                .map_err(|e| e.to_string())?
                .and_then(|terminal| terminal.conversation),
            None => None,
        };
        let origin = json!({"kind":"telegram_control","scope":caller.scope,"actor":caller.actor,"route":route,"conversation":conversation});
        let mut chunks = vec![String::new()];
        let mut units = 0;
        for c in reply.text.chars() {
            if units + c.len_utf16() > 4096 {
                chunks.push(String::new());
                units = 0;
            }
            chunks.last_mut().expect("response chunk").push(c);
            units += c.len_utf16();
        }
        let last = chunks.len() - 1;
        for (index, text) in chunks.into_iter().enumerate() {
            let mut payload = json!({"chat":chat,"text":text,"reply":reply_to});
            if index == last && !reply.buttons.is_empty() {
                payload["keyboard"] =
                    serde_json::to_value(&reply.buttons).map_err(|_| "Invalid menu buttons")?;
            }
            self.journal
                .enqueue(
                    "outbound",
                    &format!("telegram:{}:{chat}", self.bot),
                    &payload,
                    &format!("telegram:{}:{key}:{index}", self.bot),
                    Some(&origin),
                    now,
                )
                .map_err(|e| e.to_string())?;
        }
        Ok(())
    }
    /// Stages at most 100 admitted inputs. Downloads occur only after grants and
    /// exact routes validate; deterministic enqueue keys survive process failure.
    pub fn stage(
        &mut self,
        api: &mut impl Api,
        feeder: &mut impl Feeder,
        files: &mut impl Files,
        now: f64,
    ) -> Result<usize> {
        self.ready()?;
        let events = self
            .journal
            .events_page(
                "telegram",
                &self.bot.to_string(),
                &["admitted"],
                self.stage_after,
                100,
            )
            .map_err(|e| e.to_string())?;
        if events.is_empty() {
            self.stage_after = 0;
            return Ok(0);
        }
        let mut count = 0;
        for event in events {
            self.stage_after = event.id;
            if self.controls_enabled && event.payload.get("callback_query").is_some() {
                continue;
            }
            let mut message = match self.parse(&event.payload) {
                Ok(Some(m)) => m,
                Ok(None) => {
                    self.journal
                        .event_state(event.id, "ignored", None)
                        .map_err(|e| e.to_string())?;
                    continue;
                }
                Err(_) => {
                    self.journal
                        .event_state(
                            event.id,
                            "failed",
                            Some("Invalid or unsupported Telegram input"),
                        )
                        .map_err(|e| e.to_string())?;
                    continue;
                }
            };
            let scope = self.scope(message.chat);
            let actor = message.actor.to_string();
            let explicit_say = if message.text.starts_with('/') {
                match super::telegram_commands::parse(&message.text, &self.username) {
                    Ok(Some(super::telegram_commands::Command::Say(text))) => Some(text),
                    _ => None,
                }
            } else {
                None
            };
            let is_say = explicit_say.is_some();
            let command = message.text.starts_with('/') && !is_say;
            if command && self.controls_enabled {
                continue;
            }
            let permission = if command {
                Permission::Manage
            } else {
                Permission::Send
            };
            if self.routes.authorize(&scope, &actor, permission).is_err()
                || ((command || is_say) && (!message.human || message.forwarded))
            {
                self.journal
                    .event_state(event.id, "ignored", Some("Unauthorized input"))
                    .map_err(|e| e.to_string())?;
                continue;
            }
            if command {
                let first = message.text.split_whitespace().next().unwrap_or("");
                let (name, target) = first
                    .split_once('@')
                    .map_or((first, None), |(name, target)| (name, Some(target)));
                if target.is_some_and(|name| !name.eq_ignore_ascii_case(&self.username)) {
                    self.journal
                        .event_state(event.id, "ignored", None)
                        .map_err(|e| e.to_string())?;
                    continue;
                }
                // Unknown controls never become agent prompts. Shared command
                // handlers run through the scoped dispatcher before feeder staging.
                let text = match name {
                    "/help" => {
                        "Daedalus connects authorized Telegram text and files to an exact terminal route. Use /menu for available controls."
                    }
                    "/status" => {
                        "Telegram identity verified. This confirms transport identity, not agent readiness."
                    }
                    _ => "This control is unsupported; no agent action was performed.",
                };
                self.enqueue_text(
                    message.chat,
                    text,
                    Some(message.id),
                    &format!("update:{}:command", event.event_id),
                    now,
                )?;
                self.journal
                    .event_state(event.id, "handled", None)
                    .map_err(|e| e.to_string())?;
                count += 1;
                continue;
            }
            if let Some(text) = explicit_say {
                message.text = text;
            }
            let live = feeder.live()?;
            let route = match self.routes.resolve_send(&scope, &actor, &live) {
                Ok(route) => route,
                Err(_) => continue, // Retain admitted input; no fallback terminal.
            };
            let attachment = if let Some((id, name)) = &message.file {
                let bytes = match api.download(id) {
                    Ok(bytes) => bytes,
                    Err(TransportError::Invalid(_) | TransportError::Rejected(_)) => {
                        self.journal
                            .event_state(
                                event.id,
                                "failed",
                                Some("Attachment unavailable or rejected"),
                            )
                            .map_err(|e| e.to_string())?;
                        continue;
                    }
                    Err(_) => continue, // Read-only download may retry on a later pass.
                };
                Some(files.save(name, &bytes)?)
            } else {
                None
            };
            let frame = Frame {
                scope,
                actor,
                message_id: message.id,
                update_id: event
                    .event_id
                    .parse()
                    .map_err(|_| "Invalid saved update ID")?,
                terminal: route.terminal.clone(),
                route_revision: route.revision,
                conversation: self
                    .routes
                    .terminal(&route.terminal)
                    .map_err(|e| e.to_string())?
                    .and_then(|terminal| terminal.conversation),
                text: message.text,
                attachment,
            };
            let destination =
                serde_json::to_string(&frame.terminal).map_err(|_| "Invalid terminal identity")?;
            self.journal
                .enqueue(
                    "inbound",
                    &destination,
                    &serde_json::to_value(&frame).map_err(|_| "Invalid feeder frame")?,
                    &format!("telegram:{}:{}", self.bot, event.event_id),
                    Some(&json!({"channel":"telegram","bot":self.bot,"update_id":event.event_id})),
                    now,
                )
                .map_err(|e| e.to_string())?;
            self.journal
                .event_state(event.id, "handled", None)
                .map_err(|e| e.to_string())?;
            if self.feedback_acks.len() < 100 {
                self.feedback_acks.push(frame.into());
            }
            count += 1;
        }
        Ok(count)
    }
    pub fn enqueue_text(
        &mut self,
        chat: i64,
        text: &str,
        reply: Option<i64>,
        key: &str,
        now: f64,
    ) -> Result<String> {
        if chat == 0
            || text.is_empty()
            || text.encode_utf16().count() > 4096
            || text.contains('\0')
            || reply.is_some_and(|id| id <= 0)
        {
            return Err("Invalid outbound Telegram text".into());
        }
        self.journal
            .enqueue(
                "outbound",
                &format!("telegram:{}:{chat}", self.bot),
                &json!({"chat":chat,"text":text,"reply":reply}),
                &format!("telegram:{}:{key}", self.bot),
                None,
                now,
            )
            .map_err(|e| e.to_string())
    }
    /// One receipt-owned attempt. Connection failure/rate limit is retryable;
    /// uncertain responses remain visible and are never automatically replayed.
    pub fn send_one(
        &mut self,
        api: &mut impl Api,
        chat: i64,
        clock: impl Fn() -> f64,
    ) -> Result<bool> {
        match self.claim_send(chat, clock())? {
            Outbound::Empty => Ok(false),
            Outbound::Blocked => Ok(true),
            Outbound::Ready(row) => {
                let result = transmit(api, &row);
                self.complete_send(&row, result, clock())?;
                Ok(true)
            }
        }
    }
    pub fn claim_send(&mut self, chat: i64, now: f64) -> Result<Outbound> {
        self.ready()?;
        let Some(row) = self
            .journal
            .begin(
                "outbound",
                &format!("telegram:{}:{chat}", self.bot),
                "telegram",
                now,
                90.0,
            )
            .map_err(|e| e.to_string())?
        else {
            return Ok(Outbound::Empty);
        };
        let authorized = if row.origin["kind"] == "verified_hook" {
            self.hook_bindings.contains(&row.origin["binding"])
                && super::hook_effects::authorize_delivery(&row, &self.routes).is_ok()
                && self.hook_registrations.iter().any(|r| {
                    (if row.origin["effect"] == "progress" {
                        r.options.telegram_progress
                    } else {
                        r.options.telegram_delivery
                    }) && row.origin["binding"]["registration_id"].as_str() == Some(r.id.as_str())
                        && serde_json::to_value(&r.scope).ok()
                            == Some(row.origin["binding"]["goal_scope"].clone())
                })
        } else if row.origin["kind"] == "telegram_stream" {
            self.hook_bindings.contains(&row.origin["binding"])
                && self.hook_registrations.iter().any(|r| {
                    row.origin["binding"]["registration_id"].as_str() == Some(r.id.as_str())
                        && serde_json::to_value(&r.scope).ok()
                            == Some(row.origin["binding"]["goal_scope"].clone())
                })
                && self.streams.as_ref().is_some_and(|streams| {
                    streams
                        .authorize_delivery(&row, &self.routes, &self.journal)
                        .is_ok()
                })
        } else if row.origin["kind"] == "telegram_control" {
            self.control_recipient_valid(&row)
        } else {
            true
        };
        if !authorized || row.payload["chat"].as_i64() != Some(chat) {
            self.journal
                .finish(
                    &row.delivery_id,
                    row.lease_token.as_deref().ok_or("Missing delivery lease")?,
                    "draft_blocked",
                    None,
                    Some("Recipient authorization or destination changed"),
                    0.0,
                    now,
                )
                .map_err(|e| e.to_string())?;
            return Ok(Outbound::Blocked);
        }
        Ok(Outbound::Ready(Box::new(row)))
    }
    fn control_recipient_valid(&self, row: &Delivery) -> bool {
        let Ok(scope) = serde_json::from_value::<Scope>(row.origin["scope"].clone()) else {
            return false;
        };
        let Some(actor) = row.origin["actor"].as_str() else {
            return false;
        };
        if scope.instance != self.instance
            || scope.bot != self.bot.to_string()
            || scope.chat.parse::<i64>().ok() != row.payload["chat"].as_i64()
            || self
                .routes
                .authorize(&scope, actor, Permission::Read)
                .is_err()
        {
            return false;
        }
        let Ok(route) = self.routes.inspect(&scope, actor) else {
            return false;
        };
        if !row.origin["route"].is_null() {
            if serde_json::to_value(&route).ok() != Some(row.origin["route"].clone()) {
                return false;
            }
            let Some(route) = route else {
                return false;
            };
            let Ok(Some(terminal)) = self.routes.terminal(&route.terminal) else {
                return false;
            };
            if serde_json::to_value(terminal.conversation).ok()
                != Some(row.origin["conversation"].clone())
            {
                return false;
            }
        }
        true
    }
    pub fn complete_send(
        &mut self,
        row: &Delivery,
        result: std::result::Result<Value, TransportError>,
        now: f64,
    ) -> Result<()> {
        let result = result.and_then(|receipt| {
            let correlated = if let Some(callback) = row.payload["callback"].as_str() {
                receipt["callback_id"].as_str() == Some(callback)
            } else if let Some(message) = row.payload["edit_message_id"].as_i64() {
                message > 0 && receipt["message_id"].as_i64() == Some(message)
            } else {
                receipt["message_id"].as_i64().is_some_and(|id| id > 0)
            };
            if receipt["chat_id"] == row.payload["chat"] && correlated {
                Ok(receipt)
            } else {
                Err(TransportError::Uncertain)
            }
        });
        if let Ok(receipt) = &result {
            return self
                .journal
                .record_delivery_receipt(
                    &row.delivery_id,
                    row.lease_token.as_deref().ok_or("Missing delivery lease")?,
                    receipt,
                    now,
                )
                .map_err(|e| e.to_string());
        }
        let (state, receipt, error, retry) = match result {
            Ok(receipt) => ("delivered", Some(receipt), None, 0.0),
            Err(TransportError::NotSent) => (
                "not_sent",
                None,
                Some("Connection failed before submission"),
                5.0,
            ),
            Err(TransportError::RateLimited(delay)) => {
                ("not_sent", None, Some("Telegram rate limit"), delay as f64)
            }
            Err(TransportError::Invalid(_) | TransportError::Rejected(_)) => (
                "draft_blocked",
                None,
                Some("Telegram rejected this message"),
                0.0,
            ),
            Err(TransportError::Uncertain) => (
                "unknown",
                None,
                Some("Telegram outcome unknown; reconciliation required"),
                0.0,
            ),
        };
        self.journal
            .finish(
                &row.delivery_id,
                row.lease_token.as_deref().ok_or("Missing delivery lease")?,
                state,
                receipt.as_ref(),
                error,
                retry,
                now,
            )
            .map_err(|e| e.to_string())?;
        Ok(())
    }
    pub fn feed_one(
        &mut self,
        feeder: &mut impl Feeder,
        target: &TerminalId,
        clock: impl Fn() -> f64,
    ) -> Result<bool> {
        self.ready()?;
        let destination = serde_json::to_string(target).map_err(|_| "Invalid terminal identity")?;
        let Some(row) = self
            .journal
            .begin("inbound", &destination, "feeder", clock(), 90.0)
            .map_err(|e| e.to_string())?
        else {
            return Ok(false);
        };
        let frame: Frame =
            serde_json::from_value(row.payload).map_err(|_| "Invalid saved feeder frame")?;
        let live = feeder.live();
        // A restarted session is unverified before its replacement registration
        // is live. The saved frame stays queued; it is not retargeted.
        let route_bound = frame.scope.instance == self.instance
            && frame.scope.bot == self.bot.to_string()
            && frame.terminal == *target
            && self
                .routes
                .resolve_send(
                    &frame.scope,
                    &frame.actor,
                    std::slice::from_ref(&frame.terminal),
                )
                .ok()
                .is_some_and(|route| {
                    route.terminal == frame.terminal && route.revision == frame.route_revision
                });
        let verified_live = matches!(&live, Ok(ids) if ids.iter().filter(|id| *id == &frame.terminal).count() == 1);
        let (state, receipt, error, retry) = if live.is_err() {
            (
                "not_sent",
                None,
                Some("Terminal discovery failed before submission"),
                2.0,
            )
        } else if route_bound && !verified_live {
            (
                "not_sent",
                None,
                Some("Waiting for the restarted terminal session to verify; input remains queued"),
                2.0,
            )
        } else if !route_bound {
            (
                "draft_blocked",
                None,
                Some("Route changed, permission revoked or terminal unavailable"),
                0.0,
            )
        } else {
            match feeder.submit(&frame) {
                Ok(Submission::Accepted(receipt)) if !receipt.is_null() => {
                    ("accepted", Some(receipt), None, 0.0)
                }
                Ok(Submission::Busy) => (
                    "not_sent",
                    None,
                    Some("Waiting for verified agent readiness; input remains queued"),
                    2.0,
                ),
                Ok(Submission::Unavailable) => (
                    "draft_blocked",
                    None,
                    Some("Registered agent is unavailable or lacks verified readiness"),
                    0.0,
                ),
                _ => (
                    "unknown",
                    None,
                    Some("Feeder outcome unknown; reconciliation required"),
                    0.0,
                ),
            }
        };
        self.journal
            .finish(
                &row.delivery_id,
                row.lease_token.as_deref().ok_or("Missing feeder lease")?,
                state,
                receipt.as_ref(),
                error,
                retry,
                clock(),
            )
            .map_err(|e| e.to_string())?;
        if state == "accepted"
            && let Some(streams) = &mut self.streams
        {
            streams.accepted(&frame)?;
        }
        Ok(true)
    }
}
/// The network worker receives an immutable claimed draft and returns evidence;
/// it never touches the journal or retries a send.
pub fn transmit(api: &mut impl Api, row: &Delivery) -> std::result::Result<Value, TransportError> {
    let invalid = || TransportError::Invalid("Invalid outbound Telegram draft");
    let chat = row.payload["chat"]
        .as_i64()
        .filter(|id| *id != 0)
        .ok_or_else(invalid)?;
    let text = row.payload["text"]
        .as_str()
        .filter(|text| {
            !text.is_empty() && text.encode_utf16().count() <= 4096 && !text.contains('\0')
        })
        .ok_or_else(invalid)?;
    if row.direction != "outbound" || row.state != "intent" || row.lease_token.is_none() {
        return Err(invalid());
    }
    if let Some(callback) = row.payload["callback"].as_str() {
        api.answer_callback(callback, text)?;
        return Ok(json!({"chat_id":chat,"callback_id":callback}));
    }
    if let Some(message) = row.payload.get("edit_message_id") {
        let message = message.as_i64().filter(|id| *id > 0).ok_or_else(invalid)?;
        return api.edit_text(chat, message, text);
    }
    if let Some(keyboard) = row.payload.get("keyboard") {
        api.send_menu(chat, text, row.payload["reply"].as_i64(), keyboard.clone())
    } else {
        api.send_text(chat, text, row.payload["reply"].as_i64())
    }
}
fn utf16_slice(text: &str, offset: Option<u64>, length: Option<u64>) -> Option<String> {
    let offset = usize::try_from(offset?).ok()?;
    let length = usize::try_from(length?).ok()?;
    let units: Vec<_> = text.encode_utf16().collect();
    String::from_utf16(units.get(offset..offset.checked_add(length)?)?).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::{identity::Backend, routes::Terminal};
    use std::path::Path;
    struct MockApi {
        updates: Vec<Value>,
        sent: usize,
        downloads: usize,
        outcome: std::result::Result<Value, TransportError>,
        bot: i64,
    }
    impl Default for MockApi {
        fn default() -> Self {
            Self {
                updates: vec![],
                sent: 0,
                downloads: 0,
                outcome: Ok(json!({"message_id":1,"chat_id":42})),
                bot: 123,
            }
        }
    }
    impl Api for MockApi {
        fn get_me(&mut self) -> crate::adapters::telegram::Result<Value> {
            Ok(json!({"id":self.bot,"is_bot":true,"username":"example_bot"}))
        }
        fn updates(&mut self, _: i64, _: u64) -> crate::adapters::telegram::Result<Vec<Value>> {
            Ok(self.updates.clone())
        }
        fn send_text(
            &mut self,
            _: i64,
            _: &str,
            _: Option<i64>,
        ) -> crate::adapters::telegram::Result<Value> {
            self.sent += 1;
            self.outcome.clone()
        }
        fn download(&mut self, id: &str) -> crate::adapters::telegram::Result<Vec<u8>> {
            self.downloads += 1;
            if id == "expired" {
                return Err(TransportError::Rejected(400));
            }
            Ok(b"file".to_vec())
        }
        fn send_document(
            &mut self,
            _: i64,
            _: &str,
            _: Vec<u8>,
            _: &str,
        ) -> crate::adapters::telegram::Result<Value> {
            unreachable!()
        }
    }
    struct MockFeeder {
        target: TerminalId,
        sends: usize,
        outcome: Submission,
        available: bool,
    }
    impl Feeder for MockFeeder {
        fn live(&mut self) -> Result<Vec<TerminalId>> {
            if self.available {
                Ok(vec![self.target.clone()])
            } else {
                Ok(vec![])
            }
        }
        fn submit(&mut self, _: &Frame) -> Result<Submission> {
            self.sends += 1;
            Ok(self.outcome.clone())
        }
    }
    struct MockFiles;
    impl Files for MockFiles {
        fn save(&mut self, name: &str, bytes: &[u8]) -> Result<Attachment> {
            Ok(Attachment {
                path: Path::new(crate::test_support::path("/tmp/fixture.bin")).into(),
                sha256: "hash".into(),
                bytes: bytes.len(),
                name: name.into(),
            })
        }
    }
    #[test]
    fn removing_exact_hook_binding_blocks_already_queued_output() {
        use super::super::{
            hook_effects::Binding,
            hook_runtime::{Options, Registration},
            hooks::{EventKind, Harness},
        };
        let (mut engine, mut api, feeder) = fixture();
        let mut terminal = engine.routes.terminal(&feeder.target).unwrap().unwrap();
        terminal.conversation = Some("session".into());
        engine
            .routes
            .inventory(
                Backend::Tmux,
                &feeder.target.endpoint,
                &[terminal],
                true,
                true,
            )
            .unwrap();
        let registration = Registration {
            version: 1,
            id: "fixture".into(),
            directory: crate::test_support::path("/tmp/daedalus-hook-test").into(),
            scope: crate::core::goal_store::Scope {
                instance: "example".into(),
                terminal: feeder.target,
                harness: Harness::Claude,
                conversation: "session".into(),
            },
            goal_id: None,
            harness_process: None,
            server_emitter: None,
            transcript: None,
            events: vec![EventKind::Stop],
            options: Options {
                telegram_delivery: true,
                ..Default::default()
            },
            activity_lease_ms: 30000,
            pending_timeout_ms: 30000,
            hook_timeout_ms: 12000,
        };
        let scope = engine.scope(42);
        let binding = Binding {
            registration_id: registration.id.clone(),
            goal_scope: registration.scope.clone(),
            route_scope: scope.clone(),
            actor: "42".into(),
            route_revision: engine
                .routes
                .inspect(&scope, "42")
                .unwrap()
                .unwrap()
                .revision,
            reply_to: None,
            hindsight_bank: None,
        };
        engine
            .configure_hooks(vec![registration.clone()], std::slice::from_ref(&binding))
            .unwrap();
        for (key, time) in [("before", 1.0), ("after", 2.0)] {
            engine
                .journal
                .enqueue(
                    "outbound",
                    "telegram:123:42",
                    &json!({"chat":42,"text":"private result"}),
                    key,
                    Some(&json!({"kind":"verified_hook","binding":binding})),
                    time,
                )
                .unwrap();
        }
        engine.send_one(&mut api, 42, || 3.0).unwrap();
        assert_eq!(api.sent, 1);
        // Keeping the registration and route does not keep recipient consent.
        engine.configure_hooks(vec![registration], &[]).unwrap();
        engine.send_one(&mut api, 42, || 4.0).unwrap();
        assert_eq!(api.sent, 1);
        assert_eq!(
            engine
                .journal
                .delivery_for_key("outbound", "after")
                .unwrap()
                .unwrap()
                .state,
            "blocked"
        );
    }
    #[test]
    fn invalid_hook_authority_is_blocked_before_transport() {
        let (mut engine, mut api, _) = fixture();
        engine.start(&mut api).unwrap();
        engine
            .journal
            .enqueue(
                "outbound",
                "telegram:123:42",
                &json!({"chat":42,"text":"private result"}),
                "hook-test",
                Some(&json!({"kind":"verified_hook","binding":{}})),
                1.0,
            )
            .unwrap();
        assert!(engine.send_one(&mut api, 42, || 2.0).unwrap());
        assert_eq!(api.sent, 0);
        assert_eq!(
            engine
                .journal
                .delivery_for_key("outbound", "hook-test")
                .unwrap()
                .unwrap()
                .state,
            "blocked"
        );
    }
    #[test]
    fn revoked_control_recipient_and_mismatched_destination_never_send() {
        use super::super::telegram_commands::{CallbackStore, Dispatcher};
        let (mut engine, mut api, feeder) = fixture();
        let mut operations = Controls {
            terminal: engine.routes.terminal(&feeder.target).unwrap().unwrap(),
            effects: 0,
        };
        let mut dispatcher = Dispatcher {
            callbacks: CallbackStore::memory().unwrap(),
        };
        engine
            .admit_updates(vec![update(1, "/capture")], 1.0)
            .unwrap();
        engine
            .stage_controls(&mut dispatcher, &mut operations, &mut api, 2.0)
            .unwrap();
        engine
            .routes
            .set_grant(&engine.scope(42), "42", &[])
            .unwrap();
        assert!(matches!(
            engine.claim_send(42, 3.0).unwrap(),
            Outbound::Blocked
        ));
        engine
            .journal
            .enqueue(
                "outbound",
                "telegram:123:42",
                &json!({"chat":99,"text":"private"}),
                "wrong-chat",
                None,
                3.0,
            )
            .unwrap();
        assert!(matches!(
            engine.claim_send(42, 4.0).unwrap(),
            Outbound::Blocked
        ));
        assert_eq!(api.sent, 0);
    }
    #[test]
    fn late_correlated_receipt_reconciles_same_attempt_without_retry() {
        let (mut engine, mut api, _) = fixture();
        engine.enqueue_text(42, "hello", None, "late", 1.0).unwrap();
        let Outbound::Ready(row) = engine.claim_send(42, 2.0).unwrap() else {
            panic!("missing draft")
        };
        // Another queue claim may expire this intent before the owner drains HTTP.
        assert!(matches!(
            engine.claim_send(42, 100.0).unwrap(),
            Outbound::Empty
        ));
        let receipt = transmit(&mut api, &row);
        engine.complete_send(&row, receipt, 101.0).unwrap();
        assert_eq!(engine.journal.rows().unwrap()[0].state, "delivered");
        assert!(!engine.send_one(&mut api, 42, || 102.0).unwrap());
        assert_eq!(api.sent, 1);
        let mut stale = *row;
        stale.lease_token = Some("wrong-owner".into());
        assert!(
            engine
                .complete_send(&stale, Ok(json!({"chat_id":42,"message_id":1})), 103.0)
                .is_err()
        );
    }
    #[test]
    fn edit_receipt_must_match_the_requested_message() {
        let (mut engine, _, _) = fixture();
        engine
            .journal
            .enqueue(
                "outbound",
                "telegram:123:42",
                &json!({"chat":42,"text":"Working","edit_message_id":9}),
                "edit",
                None,
                1.0,
            )
            .unwrap();
        let Outbound::Ready(row) = engine.claim_send(42, 2.0).unwrap() else {
            panic!("missing edit")
        };
        engine
            .complete_send(&row, Ok(json!({"chat_id":42,"message_id":10})), 3.0)
            .unwrap();
        assert_eq!(
            engine
                .journal
                .delivery_for_key("outbound", "edit")
                .unwrap()
                .unwrap()
                .state,
            "unknown"
        );
        assert!(matches!(
            engine.claim_send(42, 4.0).unwrap(),
            Outbound::Empty
        ));
    }
    fn fixture() -> (Engine, MockApi, MockFeeder) {
        let target = TerminalId {
            backend: Backend::Tmux,
            endpoint: crate::test_support::path("/tmp/mock.sock").into(),
            pane: "%1".into(),
            generation: "generation".into(),
        };
        let mut routes = Registry::memory().unwrap();
        let scope = Scope {
            instance: "example".into(),
            bot: "123".into(),
            chat: "42".into(),
        };
        routes
            .inventory(
                Backend::Tmux,
                &target.endpoint,
                &[Terminal {
                    identity: target.clone(),
                    label: "same label".into(),
                    workspace: "".into(),
                    harness: "claude".into(),
                    conversation: None,
                    available: true,
                }],
                true,
                true,
            )
            .unwrap();
        routes
            .set_grant(
                &scope,
                "42",
                &[Permission::Manage, Permission::Send, Permission::Read],
            )
            .unwrap();
        routes
            .select(
                &scope,
                "42",
                &target,
                std::slice::from_ref(&target),
                0,
                true,
                false,
            )
            .unwrap();
        let mut engine = Engine::new(Journal::memory().unwrap(), routes, "example", 123).unwrap();
        let mut api = MockApi::default();
        engine.start(&mut api).unwrap();
        (
            engine,
            api,
            MockFeeder {
                target,
                sends: 0,
                outcome: Submission::Accepted(json!({"submitted":true})),
                available: true,
            },
        )
    }
    fn update(id: i64, text: &str) -> Value {
        json!({"update_id":id,"message":{"message_id":id+1,"chat":{"id":42,"type":"private"},"from":{"id":42,"is_bot":false},"text":text}})
    }
    struct Controls {
        terminal: Terminal,
        effects: usize,
    }
    impl super::super::telegram_commands::Operations for Controls {
        fn inventory(&mut self) -> Result<Vec<Terminal>> {
            Ok(vec![self.terminal.clone()])
        }
        fn execute(
            &mut self,
            _: &super::super::telegram_commands::Action,
            _: &super::super::telegram_commands::Target,
        ) -> Result<String> {
            self.effects += 1;
            Ok("Action submitted".into())
        }
    }
    #[test]
    fn reaction_ack_follows_durable_authorized_admission_once() {
        let (mut engine, mut api, mut feeder) = fixture();
        engine.admit_updates(vec![update(1, "hello")], 1.0).unwrap();
        engine
            .stage(&mut api, &mut feeder, &mut MockFiles, 2.0)
            .unwrap();
        assert_eq!(engine.feedback_acks.len(), 1);
        assert_eq!(engine.feedback_acks[0].message_id, 2);
        assert_eq!(engine.journal.rows().unwrap()[0].direction, "inbound");
        engine
            .stage(&mut api, &mut feeder, &mut MockFiles, 3.0)
            .unwrap();
        assert_eq!(engine.feedback_acks.len(), 1);
        engine
            .routes
            .set_grant(&engine.scope(42), "42", &[])
            .unwrap();
        engine
            .admit_updates(vec![update(2, "unauthorized")], 4.0)
            .unwrap();
        engine
            .stage(&mut api, &mut feeder, &mut MockFiles, 5.0)
            .unwrap();
        assert_eq!(engine.feedback_acks.len(), 1);
        assert_eq!(feeder.sends, 0);
    }
    #[test]
    fn explicit_say_uses_journal_once_and_obeys_busy_and_revoked_authority() {
        use super::super::telegram_commands::{CallbackStore, Dispatcher};
        let (mut engine, mut api, mut feeder) = fixture();
        let mut controls = Controls {
            terminal: engine.routes.terminal(&feeder.target).unwrap().unwrap(),
            effects: 0,
        };
        let mut dispatcher = Dispatcher {
            callbacks: CallbackStore::memory().unwrap(),
        };
        engine
            .admit_updates(vec![update(1, "/say@example_bot /review this")], 1.0)
            .unwrap();
        engine
            .stage_controls(&mut dispatcher, &mut controls, &mut api, 2.0)
            .unwrap();
        assert!(engine.journal.rows().unwrap().is_empty());
        engine
            .stage(&mut api, &mut feeder, &mut MockFiles, 3.0)
            .unwrap();
        engine
            .stage_controls(&mut dispatcher, &mut controls, &mut api, 4.0)
            .unwrap();
        engine
            .stage(&mut api, &mut feeder, &mut MockFiles, 4.0)
            .unwrap();
        let rows = engine.journal.rows().unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].direction, "inbound");
        let frame: Frame = serde_json::from_value(rows[0].payload.clone()).unwrap();
        assert_eq!(frame.text, "/review this");
        assert_eq!(frame.terminal, feeder.target);
        assert_eq!(frame.update_id, 1);
        assert_eq!(controls.effects, 0);
        assert_eq!(feeder.sends, 0);
        feeder.outcome = Submission::Busy;
        let target = feeder.target.clone();
        engine.feed_one(&mut feeder, &target, || 5.0).unwrap();
        assert_eq!(engine.journal.rows().unwrap()[0].state, "retryable");
        engine
            .routes
            .set_grant(&engine.scope(42), "42", &[])
            .unwrap();
        engine.feed_one(&mut feeder, &target, || 10.0).unwrap();
        assert_eq!(feeder.sends, 1);
        assert_eq!(engine.journal.rows().unwrap()[0].state, "blocked");
    }
    #[test]
    fn explicit_say_rejects_forwarded_bot_wrong_suffix_and_missing_send() {
        use super::super::telegram_commands::{CallbackStore, Dispatcher};
        for kind in ["forwarded", "bot", "suffix", "permission"] {
            let (mut engine, mut api, mut feeder) = fixture();
            let mut controls = Controls {
                terminal: engine.routes.terminal(&feeder.target).unwrap().unwrap(),
                effects: 0,
            };
            let mut dispatcher = Dispatcher {
                callbacks: CallbackStore::memory().unwrap(),
            };
            let mut input = update(1, "/say text");
            match kind {
                "forwarded" => input["message"]["forward_origin"] = json!({"type":"user"}),
                "bot" => input["message"]["from"]["is_bot"] = json!(true),
                "suffix" => input["message"]["text"] = json!("/say@other_bot text"),
                _ => engine
                    .routes
                    .set_grant(
                        &engine.scope(42),
                        "42",
                        &[Permission::Read, Permission::Manage],
                    )
                    .unwrap(),
            }
            engine.admit_updates(vec![input], 1.0).unwrap();
            engine
                .stage_controls(&mut dispatcher, &mut controls, &mut api, 2.0)
                .unwrap();
            engine
                .stage(&mut api, &mut feeder, &mut MockFiles, 3.0)
                .unwrap();
            assert!(
                engine
                    .journal
                    .rows()
                    .unwrap()
                    .iter()
                    .all(|row| row.direction != "inbound"),
                "{kind}"
            );
            assert_eq!(controls.effects, 0);
            assert_eq!(feeder.sends, 0);
        }
    }
    #[test]
    fn controls_flow_from_admission_through_one_use_confirmation_without_prompt_fallback() {
        use super::super::telegram_commands::{CallbackStore, Dispatcher};
        let (mut engine, mut api, mut feeder) = fixture();
        let mut controls = Controls {
            terminal: engine.routes.terminal(&feeder.target).unwrap().unwrap(),
            effects: 0,
        };
        let mut dispatcher = Dispatcher {
            callbacks: CallbackStore::memory().unwrap(),
        };
        engine
            .admit_updates(vec![update(1, "/interrupt")], 1.0)
            .unwrap();
        assert_eq!(
            engine
                .stage_controls(&mut dispatcher, &mut controls, &mut api, 2.0)
                .unwrap(),
            1
        );
        assert_eq!(controls.effects, 0);
        let row = engine.journal.rows().unwrap().remove(0);
        let token = row.payload["keyboard"][0][0]["callback_data"]
            .as_str()
            .unwrap();
        let callback = json!({"update_id":2,"callback_query":{"id":"callback-2","from":{"id":42,"is_bot":false},"data":token,"message":{"message_id":10,"from":{"id":123,"is_bot":true},"chat":{"id":42,"type":"private"}}}});
        engine.admit_updates(vec![callback.clone()], 3.0).unwrap();
        engine
            .stage_controls(&mut dispatcher, &mut controls, &mut api, 4.0)
            .unwrap();
        // Previous scan ended at its old boundary; either call may consume it.
        engine
            .stage_controls(&mut dispatcher, &mut controls, &mut api, 4.0)
            .unwrap();
        assert_eq!(controls.effects, 1);
        let mut repeated = callback;
        repeated["update_id"] = json!(3);
        repeated["callback_query"]["id"] = json!("callback-3");
        engine
            .admit_updates(vec![repeated, update(4, "/status@other_bot")], 5.0)
            .unwrap();
        engine
            .stage_controls(&mut dispatcher, &mut controls, &mut api, 6.0)
            .unwrap();
        engine
            .stage_controls(&mut dispatcher, &mut controls, &mut api, 6.0)
            .unwrap();
        engine
            .stage(&mut api, &mut feeder, &mut MockFiles, 7.0)
            .unwrap();
        assert_eq!(controls.effects, 1);
        assert_eq!(feeder.sends, 0);
        assert!(
            engine
                .journal
                .rows()
                .unwrap()
                .iter()
                .all(|r| r.direction == "outbound")
        );
    }
    #[test]
    fn restarted_processing_control_and_foreign_callback_never_execute() {
        use super::super::telegram_commands::{CallbackStore, Dispatcher};
        let (mut engine, mut api, feeder) = fixture();
        let mut controls = Controls {
            terminal: engine.routes.terminal(&feeder.target).unwrap().unwrap(),
            effects: 0,
        };
        let mut dispatcher = Dispatcher {
            callbacks: CallbackStore::memory().unwrap(),
        };
        engine
            .admit_updates(vec![update(1, "/interrupt")], 1.0)
            .unwrap();
        let event = engine
            .journal
            .events("telegram", "123", &["admitted"])
            .unwrap()
            .remove(0);
        engine
            .journal
            .event_state(event.id, "processing", None)
            .unwrap();
        engine.start(&mut api).unwrap();
        engine.admit_updates(vec![json!({"update_id":2,"callback_query":{"id":"query","from":{"id":42,"is_bot":false},"data":"forged","message":{"message_id":1,"from":{"id":999,"is_bot":true},"chat":{"id":42,"type":"private"}}}})],2.0).unwrap();
        engine
            .stage_controls(&mut dispatcher, &mut controls, &mut api, 3.0)
            .unwrap();
        assert_eq!(controls.effects, 0);
        assert_eq!(
            engine
                .journal
                .events("telegram", "123", &["unknown"])
                .unwrap()
                .len(),
            1
        );
        assert_eq!(
            engine
                .journal
                .events("telegram", "123", &["ignored"])
                .unwrap()
                .len(),
            1
        );
    }
    #[test]
    fn durable_admission_sort_duplicate_and_cursor() {
        let (mut engine, mut api, mut feeder) = fixture();
        api.updates = vec![update(9, "hello"), update(7, "first")];
        assert_eq!(engine.poll(&mut api, 1.0).unwrap(), 2);
        assert_eq!(engine.poll(&mut api, 1.0).unwrap(), 0);
        assert_eq!(engine.journal.cursor("telegram", "123", 0).unwrap(), 10);
        assert_eq!(
            engine
                .stage(&mut api, &mut feeder, &mut MockFiles, 2.0)
                .unwrap(),
            2
        );
        assert_eq!(
            engine
                .stage(&mut api, &mut feeder, &mut MockFiles, 3.0)
                .unwrap(),
            0
        );
        let target = feeder.target.clone();
        assert!(engine.feed_one(&mut feeder, &target, || 4.0).unwrap());
        assert_eq!(feeder.sends, 1);
        assert_eq!(engine.journal.rows().unwrap()[0].state, "accepted");
    }
    #[test]
    fn unauthorized_documents_never_download_and_foreign_commands_never_feed() {
        let (mut engine, mut api, mut feeder) = fixture();
        let mut denied = update(1, "text");
        denied["message"]["from"]["id"] = json!(99);
        denied["message"]["document"] = json!({"file_id":"private"});
        api.updates = vec![
            denied,
            update(2, "/status@another_bot"),
            update(3, "/unknown"),
        ];
        engine.poll(&mut api, 1.0).unwrap();
        engine
            .stage(&mut api, &mut feeder, &mut MockFiles, 2.0)
            .unwrap();
        assert_eq!(api.downloads, 0);
        assert_eq!(feeder.sends, 0);
        let rows = engine.journal.rows().unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].direction, "outbound");
    }
    #[test]
    fn restarted_session_keeps_the_exact_frame_until_it_verifies_again() {
        let (mut engine, mut api, mut feeder) = fixture();
        api.updates = vec![update(1, "hello")];
        engine.poll(&mut api, 1.0).unwrap();
        engine
            .stage(&mut api, &mut feeder, &mut MockFiles, 2.0)
            .unwrap();
        feeder.available = false;
        let target = feeder.target.clone();
        assert!(engine.feed_one(&mut feeder, &target, || 3.0).unwrap());
        assert_eq!(feeder.sends, 0);
        assert_eq!(engine.journal.rows().unwrap()[0].state, "retryable");
        feeder.available = true;
        assert!(!engine.feed_one(&mut feeder, &target, || 4.0).unwrap());
        assert!(engine.feed_one(&mut feeder, &target, || 6.0).unwrap());
        assert_eq!(feeder.sends, 1);
        assert_eq!(engine.journal.rows().unwrap()[0].state, "accepted");
    }
    fn stale_update_ids_do_not_replay_or_move_the_cursor() {
        let (mut engine, mut api, _) = fixture();
        api.updates = vec![update(4, "kept")];
        engine.poll(&mut api, 1.0).unwrap();
        assert_eq!(engine.cursor().unwrap(), 5);
        api.updates = vec![update(2, "stale replay"), update(4, "kept again")];
        assert_eq!(engine.poll(&mut api, 2.0).unwrap(), 0);
        assert_eq!(engine.cursor().unwrap(), 5);
        let events = engine.journal.events("telegram", "123", &["admitted"]).unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].payload["message"]["text"], "kept");
    }
    fn revoked_route_blocks_queued_input_without_retargeting() {
        let (mut engine, mut api, mut feeder) = fixture();
        api.updates = vec![update(1, "hello")];
        engine.poll(&mut api, 1.0).unwrap();
        engine
            .stage(&mut api, &mut feeder, &mut MockFiles, 2.0)
            .unwrap();
        engine
            .routes
            .set_grant(&engine.scope(42), "42", &[])
            .unwrap();
        let target = feeder.target.clone();
        engine.feed_one(&mut feeder, &target, || 3.0).unwrap();
        assert_eq!(feeder.sends, 0);
        assert_eq!(engine.journal.rows().unwrap()[0].state, "blocked");
    }
    #[test]
    fn uncertain_network_send_is_not_retried_and_busy_feeder_is() {
        let (mut engine, mut api, mut feeder) = fixture();
        engine
            .enqueue_text(42, "hello", None, "reply1", 1.0)
            .unwrap();
        api.outcome = Err(TransportError::Uncertain);
        assert!(engine.send_one(&mut api, 42, || 2.0).unwrap());
        assert!(!engine.send_one(&mut api, 42, || 4.0).unwrap());
        assert_eq!(api.sent, 1);
        assert_eq!(engine.journal.rows().unwrap()[0].state, "unknown");
        api.updates = vec![update(1, "hello")];
        engine.poll(&mut api, 5.0).unwrap();
        engine
            .stage(&mut api, &mut feeder, &mut MockFiles, 6.0)
            .unwrap();
        feeder.outcome = Submission::Busy;
        let target = feeder.target.clone();
        engine.feed_one(&mut feeder, &target, || 7.0).unwrap();
        assert!(!engine.feed_one(&mut feeder, &target, || 8.0).unwrap());
        assert!(engine.feed_one(&mut feeder, &target, || 10.0).unwrap());
    }
    #[test]
    fn rate_limit_delays_retry_and_wrong_bot_cannot_poll() {
        let (mut engine, mut api, _) = fixture();
        engine
            .enqueue_text(42, "hello", None, "reply", 1.0)
            .unwrap();
        api.outcome = Err(TransportError::RateLimited(10));
        engine.send_one(&mut api, 42, || 2.0).unwrap();
        assert!(!engine.send_one(&mut api, 42, || 8.0).unwrap());
        api.outcome = Ok(json!({"message_id":2}));
        engine.send_one(&mut api, 42, || 12.0).unwrap();
        assert_eq!(api.sent, 2);
        api.bot = 999;
        assert!(engine.start(&mut api).is_err());
        assert!(engine.poll(&mut api, 20.0).is_err());
    }
    #[test]
    fn saved_group_policy_overrides_defaults_without_broadening_sender_grants() {
        let (mut engine, _, _) = fixture();
        let scope = engine.scope(-42);
        engine
            .routes
            .set_grant(&scope, "42", &[Permission::Read, Permission::Manage])
            .unwrap();
        let mut input = update(1, "unmentioned group text");
        input["message"]["chat"] = json!({"id":-42,"type":"supergroup"});
        assert!(engine.parse(&input).unwrap().is_none());
        engine
            .routes
            .set_input_policy(&scope, "42", 0, true)
            .unwrap();
        assert!(engine.parse(&input).unwrap().is_some());
        assert!(
            engine
                .routes
                .authorize(&scope, "42", Permission::Send)
                .is_err()
        );
        engine.listen_chats.insert(-42);
        engine
            .routes
            .set_input_policy(&scope, "42", 1, false)
            .unwrap();
        assert!(engine.parse(&input).unwrap().is_none());
        input["message"]["reply_to_message"] = json!({"from":{"id":123}});
        assert!(engine.parse(&input).unwrap().is_some());
        input["message"]["chat"] = json!({"id":-43,"type":"supergroup"});
        input["message"]
            .as_object_mut()
            .unwrap()
            .remove("reply_to_message");
        assert!(engine.parse(&input).unwrap().is_none());
        input["message"]["chat"] = json!({"id":42,"type":"private"});
        assert!(engine.parse(&input).unwrap().is_some());
    }
    #[test]
    fn group_mentions_use_utf16_entities_and_require_exact_bot() {
        let (engine, _, _) = fixture();
        let mut input = update(1, "😀 @example_bot hello");
        input["message"]["chat"] = json!({"id":-42,"type":"supergroup"});
        assert!(engine.parse(&input).unwrap().is_none());
        input["message"]["entities"] = json!([{"type":"mention","offset":3,"length":12}]);
        assert!(engine.parse(&input).unwrap().is_some());
        input["message"]["entities"][0]["offset"] = json!(2);
        assert!(engine.parse(&input).unwrap().is_none());
    }
    #[test]
    fn conflicting_batch_does_not_advance_cursor() {
        let (mut engine, mut api, _) = fixture();
        api.updates = vec![update(1, "a"), update(1, "b")];
        assert!(engine.poll(&mut api, 1.0).is_err());
        assert_eq!(engine.journal.cursor("telegram", "123", 0).unwrap(), 0);
    }
    #[test]
    fn stale_routes_do_not_starve_later_commands_and_bad_files_do_not_poison_queue() {
        let (mut engine, mut api, mut feeder) = fixture();
        let unresolved = Scope {
            instance: "example".into(),
            bot: "123".into(),
            chat: "99".into(),
        };
        engine
            .routes
            .set_grant(&unresolved, "42", &[Permission::Send])
            .unwrap();
        api.updates = (0..100)
            .map(|id| {
                let mut row = update(id, "pending");
                row["message"]["chat"]["id"] = json!(99);
                row
            })
            .collect();
        engine.poll(&mut api, 1.0).unwrap();
        api.updates = vec![update(100, "/help")];
        engine.poll(&mut api, 2.0).unwrap();
        assert_eq!(
            engine
                .stage(&mut api, &mut feeder, &mut MockFiles, 3.0)
                .unwrap(),
            0
        );
        assert_eq!(
            engine
                .stage(&mut api, &mut feeder, &mut MockFiles, 4.0)
                .unwrap(),
            1
        );
        let mut bad = update(101, "file");
        bad["message"]["document"] = json!({"file_id":"expired"});
        api.updates = vec![bad, update(102, "good")];
        engine.poll(&mut api, 5.0).unwrap();
        assert_eq!(
            engine
                .stage(&mut api, &mut feeder, &mut MockFiles, 6.0)
                .unwrap(),
            1
        );
        assert_eq!(api.downloads, 1);
        assert_eq!(
            engine
                .journal
                .events("telegram", "123", &["failed"])
                .unwrap()
                .len(),
            1
        );
    }
    #[cfg(unix)]
    #[test]
    fn attachment_store_accepts_large_files_without_using_untrusted_names_as_paths() {
        use std::os::unix::fs::MetadataExt;
        let directory =
            std::env::temp_dir().join(format!("daedalus-attachment-test-{}", std::process::id()));
        let mut files = PrivateFiles {
            directory: directory.clone(),
        };
        let bytes = vec![0x61; 9 * 1024 * 1024];
        let saved = files.save("../../name\n.txt", &bytes).unwrap();
        assert_eq!(saved.path.parent(), Some(directory.as_path()));
        assert!(!saved.name.contains('/'));
        assert!(!saved.name.contains('\n'));
        assert_eq!(
            std::fs::metadata(&saved.path).unwrap().mode() & 0o777,
            0o600
        );
        assert_eq!(
            files.save("different.txt", &bytes).unwrap().path,
            saved.path
        );
        assert!(saved.path.to_string_lossy().ends_with(".txt"));
        let photo = files.save("photo.JPG", b"jpeg").unwrap();
        assert_eq!(photo.path.extension().unwrap(), "jpg");
        for name in ["archive", "x.tar.g$", "x.verylongextension", "trailing."] {
            let saved = files.save(name, name.as_bytes()).unwrap();
            assert_eq!(saved.path.extension().unwrap(), "bin");
        }
        std::fs::remove_dir_all(directory).unwrap();
    }
}
