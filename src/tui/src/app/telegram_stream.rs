//! Opt-in DM event streaming. Only accepted Telegram prompt turns may expose
//! verified assistant text and tool names. No transcript scraping or token claims.
use super::{
    hook_effects::{self, Binding},
    hook_runtime::Registration,
    telegram::Frame,
    telegram_commands::Caller,
    telegram_config::ServiceConfig,
};
use crate::core::{
    journal::{Delivery, Event, Journal},
    routes::{Permission, Registry},
};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::path::Path;
type Result<T> = std::result::Result<T, String>;
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    Text,
    Work,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Setting {
    On,
    Off,
    Status,
}
pub fn setting(value: &str) -> Result<Setting> {
    match value {
        "on" => Ok(Setting::On),
        "off" => Ok(Setting::Off),
        "status" | "" => Ok(Setting::Status),
        _ => Err("Use on, off or status".into()),
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Plan {
    pub binding: Binding,
    pub mode: Mode,
}
impl Plan {
    pub fn description(&self) -> String {
        format!(
            "Enable {} for future accepted Telegram turns in this private chat. Updates arrive at verified hook event boundaries, not token-by-token. No historical text, hidden reasoning, tool arguments or results are exported. Another chat or local prompt ends this turn's output. /stream off stops queued updates.",
            if self.mode == Mode::Work {
                "assistant text and tool names"
            } else {
                "assistant text"
            }
        )
    }
}
pub fn authorize_caller(caller: &Caller, routes: &Registry) -> Result<()> {
    if !caller.human
        || caller.forwarded
        || caller
            .scope
            .chat
            .parse::<i64>()
            .ok()
            .filter(|c| *c > 0)
            .is_none()
        || caller.scope.chat != caller.actor
    {
        return Err("Streaming requires your private direct chat".into());
    }
    for permission in [Permission::Read, Permission::Send, Permission::Manage] {
        routes
            .authorize(&caller.scope, &caller.actor, permission)
            .map_err(|_| "Streaming requires Read, Send and Manage")?;
    }
    Ok(())
}
fn authorize(plan: &Plan, routes: &Registry) -> Result<()> {
    let b = &plan.binding;
    let caller = Caller {
        scope: b.route_scope.clone(),
        actor: b.actor.clone(),
        human: true,
        forwarded: false,
        bot_username: String::new(),
        event_id: String::new(),
    };
    authorize_caller(&caller, routes)?;
    hook_effects::authorize(b, routes)
}
fn scope_key(caller: &Caller) -> Result<String> {
    Ok(format!(
        "{:x}",
        Sha256::digest(
            serde_json::to_vec(&(&caller.scope, &caller.actor))
                .map_err(|_| "Invalid stream scope")?
        )
    ))
}
fn key(plan: &Plan) -> Result<String> {
    scope_key(&Caller {
        scope: plan.binding.route_scope.clone(),
        actor: plan.binding.actor.clone(),
        human: true,
        forwarded: false,
        bot_username: String::new(),
        event_id: String::new(),
    })
}
const SCHEMA:&str="PRAGMA synchronous=FULL;
CREATE TABLE IF NOT EXISTS stream_subscriptions(id TEXT PRIMARY KEY,plan TEXT NOT NULL,epoch INTEGER NOT NULL,enabled INTEGER NOT NULL,after_id INTEGER NOT NULL,prompt_id INTEGER NOT NULL,turn TEXT,last_text TEXT,input_after INTEGER NOT NULL,diagnostic TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS stream_inputs(hash TEXT PRIMARY KEY,scope TEXT NOT NULL,actor TEXT NOT NULL,terminal TEXT NOT NULL,conversation TEXT,revision INTEGER NOT NULL,used_event TEXT);
";
pub struct Store {
    con: Connection,
}
#[derive(Clone)]
struct Subscription {
    id: String,
    plan: Plan,
    epoch: i64,
    after: i64,
    prompt: i64,
    turn: Option<String>,
    last_text: Option<String>,
    input_after: i64,
    diagnostic: String,
}
impl Store {
    pub fn open(directory: &Path) -> Result<Self> {
        Self::from_connection(super::telegram_config::private_connection(
            &directory.join("telegram-stream.sqlite3"),
        )?)
    }
    pub fn memory() -> Result<Self> {
        Self::from_connection(
            Connection::open_in_memory().map_err(|_| "Cannot create stream store")?,
        )
    }
    fn from_connection(con: Connection) -> Result<Self> {
        con.execute_batch(SCHEMA)
            .map_err(|_| "Cannot initialize stream store")?;
        Ok(Self { con })
    }
    pub fn start(&mut self, plan: &Plan, head: i64) -> Result<String> {
        if head < 0
            || plan.binding.route_scope.chat != plan.binding.actor
            || plan
                .binding
                .actor
                .parse::<u64>()
                .ok()
                .filter(|v| *v > 0)
                .is_none()
        {
            return Err("Invalid stream boundary or DM identity".into());
        }
        let encoded = serde_json::to_string(plan).map_err(|_| "Cannot encode stream binding")?;
        let input_after: i64 = self
            .con
            .query_row(
                "SELECT COALESCE(MAX(rowid),0) FROM stream_inputs",
                [],
                |r| r.get(0),
            )
            .map_err(|_| "Cannot freeze stream input boundary")?;
        self.con.execute("INSERT INTO stream_subscriptions VALUES(?,?,1,1,?,0,NULL,NULL,?,'waiting_for_future_prompt') ON CONFLICT(id) DO UPDATE SET plan=excluded.plan,epoch=epoch+1,enabled=1,after_id=excluded.after_id,prompt_id=0,turn=NULL,last_text=NULL,input_after=excluded.input_after,diagnostic=excluded.diagnostic",params![key(plan)?,encoded,head,input_after]).map_err(|_|"Cannot persist stream start")?;
        Ok("Streaming enabled for future accepted Telegram turns only. Cadence follows verified assistant/tool hooks; it is not token streaming. No previous transcript will be replayed.".into())
    }
    pub fn control(&mut self, caller: &Caller, setting: Setting) -> Result<String> {
        let id = scope_key(caller)?;
        if setting == Setting::Off {
            self.con
                .execute(
                    "UPDATE stream_subscriptions SET enabled=0,epoch=epoch+1,turn=NULL WHERE id=?",
                    [id],
                )
                .map_err(|_| "Cannot stop stream")?;
            return Ok("Streaming stopped for this private chat; queued stream updates are revoked. A request already sent to Telegram cannot be recalled.".into());
        }
        if setting != Setting::Status {
            return Err("Stream start requires confirmation".into());
        }
        let row: Option<(bool, String)> = self
            .con
            .query_row(
                "SELECT enabled,diagnostic FROM stream_subscriptions WHERE id=?",
                [id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()
            .map_err(|_| "Cannot inspect stream")?;
        Ok(match row {
            Some((true, evidence)) => format!(
                "Streaming enabled. Latest evidence: {evidence}. Updates follow verified event boundaries, not tokens; enabled is not proof the agent is running."
            ),
            _ => "Streaming is off.".into(),
        })
    }
    pub fn accepted(&mut self, frame: &Frame) -> Result<()> {
        let (text, _) = super::telegram_feeder::encode_prompt(frame)?;
        let hash = format!("{:x}", Sha256::digest(text.as_bytes()));
        self.con
            .execute(
                "INSERT OR IGNORE INTO stream_inputs VALUES(?,?,?,?,?,?,NULL)",
                params![
                    hash,
                    serde_json::to_string(&frame.scope).map_err(|_| "Invalid stream scope")?,
                    frame.actor,
                    serde_json::to_string(&frame.terminal).map_err(|_| "Invalid stream target")?,
                    frame.conversation,
                    frame.route_revision
                ],
            )
            .map_err(|_| "Cannot record accepted prompt provenance")?;
        // Bounded recent acceptance ledger. Pruning can only suppress output,
        // never select historical prompts as a fallback.
        self.con.execute("DELETE FROM stream_inputs WHERE rowid NOT IN (SELECT rowid FROM stream_inputs ORDER BY rowid DESC LIMIT 10000)",[]).map_err(|_|"Cannot bound stream provenance")?;
        Ok(())
    }
    fn subscriptions(&self) -> Result<Vec<Subscription>> {
        let mut statement=self.con.prepare("SELECT id,plan,epoch,after_id,prompt_id,turn,last_text,input_after,diagnostic FROM stream_subscriptions WHERE enabled=1 LIMIT 1025").map_err(|_|"Cannot inspect streams")?;
        let rows = statement
            .query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get(2)?,
                    r.get(3)?,
                    r.get(4)?,
                    r.get(5)?,
                    r.get(6)?,
                    r.get(7)?,
                    r.get(8)?,
                ))
            })
            .map_err(|_| "Cannot inspect streams")?;
        let mut subscriptions = Vec::new();
        for row in rows {
            let (id, plan, epoch, after, prompt, turn, last_text, input_after, diagnostic) =
                row.map_err(|_| "Cannot inspect stream")?;
            subscriptions.push(Subscription {
                id,
                plan: serde_json::from_str(&plan).map_err(|_| "Invalid stream plan")?,
                epoch,
                after,
                prompt,
                turn,
                last_text,
                input_after,
                diagnostic,
            });
        }
        if subscriptions.len() > 1024 {
            return Err("Stream subscription limit exceeded".into());
        }
        Ok(subscriptions)
    }
    fn configured(
        plan: &Plan,
        config: &ServiceConfig,
        registrations: &[(std::path::PathBuf, Registration)],
    ) -> bool {
        config.hook_bindings.iter().any(|b| b == &plan.binding)
            && registrations.iter().any(|(_, r)| {
                r.id == plan.binding.registration_id && r.scope == plan.binding.goal_scope
            })
    }
    pub fn tick(
        &mut self,
        journal: &mut Journal,
        routes: &Registry,
        config: &ServiceConfig,
        registrations: &[(std::path::PathBuf, Registration)],
        now: f64,
    ) -> Result<()> {
        for mut subscription in self.subscriptions()? {
            if !Self::configured(&subscription.plan, config, registrations)
                || authorize(&subscription.plan, routes).is_err()
            {
                self.con
                    .execute(
                        "UPDATE stream_subscriptions SET enabled=0,epoch=epoch+1 WHERE id=?",
                        [&subscription.id],
                    )
                    .map_err(|_| "Cannot revoke stream")?;
                continue;
            }
            let registration = registrations
                .iter()
                .find(|(_, r)| r.id == subscription.plan.binding.registration_id)
                .ok_or("Stream registration unavailable")?
                .1
                .clone();
            let events = journal
                .events_page(
                    "hooks",
                    &config.instance,
                    &["admitted", "handled"],
                    subscription.after,
                    64,
                )
                .map_err(|e| e.to_string())?;
            for event in events {
                self.event(&mut subscription, &event, &registration, journal, now)?;
                self.con.execute("UPDATE stream_subscriptions SET after_id=?,prompt_id=?,turn=?,last_text=?,diagnostic=? WHERE id=? AND epoch=? AND enabled=1",params![event.id,subscription.prompt,subscription.turn,subscription.last_text,subscription.diagnostic,subscription.id,subscription.epoch]).map_err(|_|"Cannot advance stream boundary")?;
                subscription.after = event.id;
            }
        }
        Ok(())
    }
    fn accepted_prompt(&self, subscription: &Subscription, event: &Event) -> Result<bool> {
        let b = &subscription.plan.binding;
        let Some(hash) = event.payload["stream_prompt_sha256"].as_str() else {
            return Ok(false);
        };
        if hash.len() != 64 || !hash.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Ok(false);
        }
        let matched=self.con.query_row("SELECT EXISTS(SELECT 1 FROM stream_inputs WHERE hash=? AND scope=? AND actor=? AND terminal=? AND conversation=? AND revision=? AND rowid>? AND (used_event IS NULL OR used_event=?))",params![hash,serde_json::to_string(&b.route_scope).map_err(|_|"Invalid stream scope")?,b.actor,serde_json::to_string(&b.goal_scope.terminal).map_err(|_|"Invalid stream terminal")?,b.goal_scope.conversation,b.route_revision,subscription.input_after,event.event_id],|r|r.get::<_,bool>(0)).map_err(|_|"Cannot verify prompt provenance")?;
        if matched {
            self.con.execute("UPDATE stream_inputs SET used_event=? WHERE hash=? AND (used_event IS NULL OR used_event=?)",params![event.event_id,hash,event.event_id]).map_err(|_|"Cannot consume prompt provenance")?;
        }
        Ok(matched)
    }
    fn event(
        &mut self,
        s: &mut Subscription,
        event: &Event,
        registration: &Registration,
        journal: &mut Journal,
        now: f64,
    ) -> Result<()> {
        if event.payload["registration_id"] != registration.id
            || event.payload["scope"] != json!(registration.scope)
            || event.payload["transcript"] != json!(registration.transcript)
        {
            return Ok(());
        }
        let kind =
            serde_json::from_value::<super::hooks::EventKind>(event.payload["event"].clone())
                .map_err(|_| "Invalid stream event")?;
        if !registration.events.contains(&kind) {
            return Ok(());
        }
        let turn = event.payload["stream_turn_id"]
            .as_str()
            .filter(|v| !v.is_empty() && v.len() <= 512 && !v.chars().any(char::is_control));
        if kind == super::hooks::EventKind::UserPromptSubmit {
            s.prompt = event.id;
            s.last_text = None;
            s.turn = None;
            s.diagnostic = "prompt_not_owned_by_this_stream".into();
            if self.accepted_prompt(s, event)? {
                s.turn = turn.map(str::to_owned);
                s.diagnostic = if turn.is_some() {
                    "verified_event_turn"
                } else {
                    match event.payload["stream_evidence"].as_str() {
                        Some("missing_or_ambiguous_transcript_ancestry") => {
                            "missing_or_ambiguous_transcript_ancestry"
                        }
                        _ => "missing_native_turn_identity",
                    }
                }
                .into();
            }
            return Ok(());
        }
        if s.turn.is_none() || s.turn.as_deref() != turn {
            return Ok(());
        }
        let text = match kind {
            super::hooks::EventKind::AssistantResponse | super::hooks::EventKind::Stop => event
                .payload["final_message"]
                .as_str()
                .filter(|s| !s.is_empty())
                .map(str::to_owned),
            super::hooks::EventKind::PostToolUse if s.plan.mode == Mode::Work => {
                event.payload["tool_name"]
                    .as_str()
                    .filter(|n| {
                        !n.is_empty()
                            && n.len() <= 120
                            && n.bytes()
                                .all(|b| b.is_ascii_alphanumeric() || b"_.:-".contains(&b))
                    })
                    .map(|name| format!("Tool: {name}"))
            }
            _ => None,
        };
        let Some(text) = text else { return Ok(()) };
        if text.len() > 64000 || text.contains('\0') {
            return Ok(());
        }
        let is_text = kind != super::hooks::EventKind::PostToolUse;
        // Ordinary reviewed hook delivery owns its text event. Streaming must
        // never add a second copy or suppress normal notifications when stream
        // evidence is missing/revoked. Tool-name events have no such duplicate.
        let normal_effect = match kind {
            super::hooks::EventKind::Stop => "queue_final_reply",
            super::hooks::EventKind::AssistantResponse => "queue_assistant_reply",
            _ => "",
        };
        if is_text
            && registration.options.telegram_delivery
            && event.payload["effects"]
                .as_array()
                .is_some_and(|effects| effects.iter().any(|v| v.as_str() == Some(normal_effect)))
        {
            s.diagnostic = "normal_hook_delivery_owns_text_event".into();
            return Ok(());
        }
        let sha = format!("{:x}", Sha256::digest(text.as_bytes()));
        if is_text && s.last_text.as_deref() == Some(&sha) {
            return Ok(());
        }
        let mut chunks = Vec::new();
        let mut chunk = String::new();
        let mut length = 0;
        for c in text
            .chars()
            .filter(|c| !c.is_control() || *c == '\n' || *c == '\t')
        {
            let n = c.len_utf16();
            if length + n > 4000 {
                chunks.push(std::mem::take(&mut chunk));
                length = 0;
            }
            chunk.push(c);
            length += n;
        }
        if !chunk.is_empty() {
            chunks.push(chunk)
        }
        let binding = &s.plan.binding;
        let chat = binding
            .route_scope
            .chat
            .parse::<i64>()
            .map_err(|_| "Invalid stream chat")?;
        for (index, part) in chunks.into_iter().enumerate() {
            journal.enqueue("outbound",&format!("telegram:{}:{chat}",binding.route_scope.bot),&json!({"chat":chat,"text":part}),&format!("stream:{}:{}:{}:{index}",s.id,s.epoch,event.id),Some(&json!({"kind":"telegram_stream","binding":binding,"subscription":s.id,"epoch":s.epoch,"turn":s.turn,"prompt_id":s.prompt,"event_id":event.event_id})),now).map_err(|e|e.to_string())?;
        }
        if is_text {
            s.last_text = Some(sha);
        }
        Ok(())
    }
    pub fn authorize_delivery(
        &self,
        row: &Delivery,
        routes: &Registry,
        journal: &Journal,
    ) -> Result<()> {
        if row.origin["kind"] != "telegram_stream" {
            return Err("Invalid stream delivery".into());
        }
        let id = row.origin["subscription"]
            .as_str()
            .ok_or("Missing stream identity")?;
        let subscription = self
            .subscriptions()?
            .into_iter()
            .find(|s| s.id == id)
            .ok_or("Stream stopped")?;
        let b = &subscription.plan.binding;
        authorize(&subscription.plan, routes)?;
        if row.origin["binding"] != json!(b)
            || row.origin["epoch"].as_i64() != Some(subscription.epoch)
            || row.origin["turn"].as_str() != subscription.turn.as_deref()
            || subscription.turn.is_none()
            || row.origin["prompt_id"].as_i64() != Some(subscription.prompt)
            || row.destination != format!("telegram:{}:{}", b.route_scope.bot, b.route_scope.chat)
            || row.payload["chat"].as_i64() != b.route_scope.chat.parse::<i64>().ok()
        {
            return Err("Stream provenance changed".into());
        }
        let latest = journal
            .latest_hook_prompt(&b.goal_scope.instance, &json!(b.goal_scope))
            .map_err(|e| e.to_string())?
            .ok_or("No current prompt")?;
        if latest.id != subscription.prompt
            || latest.payload["stream_turn_id"].as_str() != subscription.turn.as_deref()
        {
            return Err("A newer user prompt closed this stream turn".into());
        }
        Ok(())
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::{
        app::{
            hook_runtime::Options,
            hooks::{EventKind, Harness},
        },
        core::{
            goal_store,
            identity::{Backend, TerminalId},
            routes::{Scope, Terminal},
        },
    };
    struct Fixture {
        store: Store,
        journal: Journal,
        routes: Registry,
        config: ServiceConfig,
        registration: Registration,
        plan: Plan,
        frame: Frame,
    }
    impl Fixture {
        fn new() -> Self {
            let terminal = TerminalId {
                backend: Backend::Herdr,
                endpoint: "fixture".into(),
                pane: "pane".into(),
                generation: "generation".into(),
            };
            let goal = goal_store::Scope {
                instance: "fixture".into(),
                terminal: terminal.clone(),
                conversation: "conversation".into(),
                harness: Harness::Pi,
            };
            let scope = Scope {
                instance: "fixture".into(),
                bot: "1".into(),
                chat: "2".into(),
            };
            let registration = Registration {
                version: 1,
                id: "hook".into(),
                directory: crate::test_support::path("/tmp/unused-stream-instance").into(),
                scope: goal.clone(),
                goal_id: None,
                harness_process: None,
                server_emitter: None,
                transcript: None,
                events: vec![
                    EventKind::UserPromptSubmit,
                    EventKind::AssistantResponse,
                    EventKind::Stop,
                    EventKind::PostToolUse,
                ],
                options: Options::default(),
                activity_lease_ms: 30000,
                pending_timeout_ms: 30000,
                hook_timeout_ms: 12000,
            };
            let mut routes = Registry::memory().unwrap();
            routes
                .inventory(
                    Backend::Herdr,
                    "fixture",
                    &[Terminal {
                        identity: terminal.clone(),
                        label: "fixture".into(),
                        workspace: crate::test_support::path("/tmp").into(),
                        harness: "pi".into(),
                        conversation: Some("conversation".into()),
                        available: true,
                    }],
                    true,
                    true,
                )
                .unwrap();
            routes
                .set_grant(
                    &scope,
                    "2",
                    &[Permission::Read, Permission::Send, Permission::Manage],
                )
                .unwrap();
            routes
                .select(
                    &scope,
                    "2",
                    &terminal,
                    std::slice::from_ref(&terminal),
                    0,
                    true,
                    false,
                )
                .unwrap();
            let binding = Binding {
                registration_id: "hook".into(),
                goal_scope: goal,
                route_scope: scope.clone(),
                actor: "2".into(),
                route_revision: 1,
                reply_to: None,
                hindsight_bank: None,
            };
            let config = ServiceConfig {
                version: 1,
                instance: "fixture".into(),
                bot_id: 1,
                token_file: crate::test_support::path("/tmp/unused-token").into(),
                grants: vec![],
                listen_chats: Default::default(),
                registrations: vec![],
                hook_bindings: vec![binding.clone()],
            };
            let plan = Plan {
                binding,
                mode: Mode::Work,
            };
            let frame = Frame {
                scope,
                actor: "2".into(),
                message_id: 3,
                update_id: 4,
                terminal,
                route_revision: 1,
                conversation: Some("conversation".into()),
                text: "request".into(),
                attachment: None,
            };
            Self {
                store: Store::memory().unwrap(),
                journal: Journal::memory().unwrap(),
                routes,
                config,
                registration,
                plan,
                frame,
            }
        }
        fn caller(&self) -> Caller {
            Caller {
                scope: self.frame.scope.clone(),
                actor: self.frame.actor.clone(),
                human: true,
                forwarded: false,
                bot_username: "fixture_bot".into(),
                event_id: "4".into(),
            }
        }
        fn start(&mut self) {
            self.store
                .start(&self.plan, self.journal.hook_head("fixture").unwrap())
                .unwrap();
        }
        fn accept(&mut self) {
            self.store.accepted(&self.frame).unwrap();
        }
        fn event(
            &mut self,
            id: &str,
            kind: EventKind,
            turn: Option<&str>,
            prompt: Option<&Frame>,
            text: &str,
        ) {
            let hash = prompt.map(|f| {
                format!(
                    "{:x}",
                    Sha256::digest(
                        super::super::telegram_feeder::encode_prompt(f)
                            .unwrap()
                            .0
                            .as_bytes()
                    )
                )
            });
            let payload = json!({"registration_id":"hook","scope":self.registration.scope,"event":kind,"transcript":null,"stream_turn_id":turn,"stream_prompt_sha256":hash,"final_message":text,"tool_name":"Read","tool_input":{"secret":"NEVER_EXPORT_ARGUMENTS"},"tool_result":"NEVER_EXPORT_RESULTS","reasoning":"NEVER_EXPORT_REASONING"});
            self.journal
                .admit("hooks", "fixture", id, &payload, 1.0)
                .unwrap();
        }
        fn prompt(&mut self, id: &str, turn: &str) {
            self.event(
                id,
                EventKind::UserPromptSubmit,
                Some(turn),
                Some(&self.frame.clone()),
                "",
            );
        }
        fn tick(&mut self) {
            self.store
                .tick(
                    &mut self.journal,
                    &self.routes,
                    &self.config,
                    &[(
                        crate::test_support::path("/tmp/unused").into(),
                        self.registration.clone(),
                    )],
                    2.0,
                )
                .unwrap();
        }
        fn rows(&self) -> Vec<Delivery> {
            self.journal.rows().unwrap()
        }
    }
    #[test]
    fn future_native_events_stream_only_assistant_text_and_tool_names_once() {
        let mut f = Fixture::new();
        f.start();
        f.accept();
        f.prompt("prompt", "turn");
        f.event("tool", EventKind::PostToolUse, Some("turn"), None, "");
        f.event(
            "answer",
            EventKind::AssistantResponse,
            Some("turn"),
            None,
            "visible answer",
        );
        f.event(
            "stop",
            EventKind::Stop,
            Some("turn"),
            None,
            "visible answer",
        );
        f.tick();
        f.tick();
        let rows = f.rows();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].payload["text"], "Tool: Read");
        assert_eq!(rows[1].payload["text"], "visible answer");
        for row in rows {
            assert!(
                !serde_json::to_string(&row.payload)
                    .unwrap()
                    .contains("NEVER_EXPORT")
            );
            f.store
                .authorize_delivery(&row, &f.routes, &f.journal)
                .unwrap();
        }
    }
    #[test]
    fn old_history_and_unaccepted_or_reused_prompt_are_never_exported() {
        let mut f = Fixture::new();
        f.accept();
        f.prompt("old-prompt", "old-turn");
        f.event(
            "old-answer",
            EventKind::Stop,
            Some("old-turn"),
            None,
            "old secret",
        );
        f.start();
        f.tick();
        assert!(f.rows().is_empty());
        // Copying an old accepted envelope after opt-in cannot revive it.
        f.prompt("copied", "copied-turn");
        f.event(
            "copied-answer",
            EventKind::Stop,
            Some("copied-turn"),
            None,
            "old secret",
        );
        f.tick();
        assert!(f.rows().is_empty());
        f.frame.update_id += 1;
        f.accept();
        f.prompt("new", "new-turn");
        f.event(
            "new-answer",
            EventKind::Stop,
            Some("new-turn"),
            None,
            "new text",
        );
        f.tick();
        assert_eq!(f.rows().len(), 1);
        f.prompt("reused", "reused-turn");
        f.event(
            "reused-answer",
            EventKind::Stop,
            Some("reused-turn"),
            None,
            "must stay local",
        );
        f.tick();
        assert_eq!(f.rows().len(), 1);
    }
    #[test]
    fn another_chat_or_local_prompt_revokes_queued_output_before_tick() {
        let mut f = Fixture::new();
        f.start();
        f.accept();
        f.prompt("first", "turn1");
        f.event("answer", EventKind::Stop, Some("turn1"), None, "answer");
        f.tick();
        let row = f.rows().remove(0);
        let mut other = f.frame.clone();
        other.scope.chat = "9".into();
        other.actor = "9".into();
        other.update_id = 10;
        f.store.accepted(&other).unwrap();
        f.event(
            "other-chat",
            EventKind::UserPromptSubmit,
            Some("turn2"),
            Some(&other),
            "",
        );
        assert!(
            f.store
                .authorize_delivery(&row, &f.routes, &f.journal)
                .is_err()
        );
        f.event(
            "other-answer",
            EventKind::Stop,
            Some("turn2"),
            None,
            "other chat secret",
        );
        f.tick();
        assert_eq!(f.rows().len(), 1);
        f.event(
            "local",
            EventKind::UserPromptSubmit,
            Some("local-turn"),
            None,
            "",
        );
        f.event(
            "local-answer",
            EventKind::Stop,
            Some("local-turn"),
            None,
            "local secret",
        );
        f.tick();
        assert_eq!(f.rows().len(), 1);
    }
    #[test]
    fn stop_and_route_or_grant_changes_revoke_already_queued_updates() {
        for reason in ["stop", "route", "grant"] {
            let mut f = Fixture::new();
            f.start();
            f.accept();
            f.prompt("prompt", "turn");
            f.event("answer", EventKind::Stop, Some("turn"), None, "answer");
            f.tick();
            let row = f.rows().remove(0);
            match reason {
                "stop" => {
                    let caller = f.caller();
                    f.store.control(&caller, Setting::Off).unwrap();
                }
                "route" => {
                    f.routes.set_pinned(&f.frame.scope, "2", 1, true).unwrap();
                }
                _ => f
                    .routes
                    .set_grant(&f.frame.scope, "2", &[Permission::Read])
                    .unwrap(),
            }
            assert!(
                f.store
                    .authorize_delivery(&row, &f.routes, &f.journal)
                    .is_err(),
                "{reason}"
            );
            f.tick();
            assert_eq!(f.rows().len(), 1);
        }
    }
    #[test]
    fn missing_turn_evidence_fails_closed_and_dm_permissions_are_explicit() {
        let mut f = Fixture::new();
        let mut caller = f.caller();
        authorize_caller(&caller, &f.routes).unwrap();
        caller.scope.chat = "-100".into();
        assert!(authorize_caller(&caller, &f.routes).is_err());
        caller = f.caller();
        caller.forwarded = true;
        assert!(authorize_caller(&caller, &f.routes).is_err());
        f.routes
            .set_grant(&f.frame.scope, "2", &[Permission::Read, Permission::Send])
            .unwrap();
        assert!(authorize_caller(&f.caller(), &f.routes).is_err());
        f = Fixture::new();
        f.start();
        f.accept();
        f.event(
            "prompt",
            EventKind::UserPromptSubmit,
            None,
            Some(&f.frame.clone()),
            "",
        );
        f.event("answer", EventKind::Stop, None, None, "never");
        f.tick();
        assert!(f.rows().is_empty());
        assert!(
            f.store
                .control(&f.caller(), Setting::Status)
                .unwrap()
                .contains("missing_native_turn_identity")
        );
    }
    #[test]
    fn engine_admission_accepted_prompt_hook_outbox_and_transport_share_authority() {
        use super::super::telegram::{Engine, Feeder, PrivateFiles, Submission};
        use crate::adapters::telegram::{Api, Result as TransportResult};
        struct ApiFixture {
            sent: Vec<String>,
        }
        impl Api for ApiFixture {
            fn get_me(&mut self) -> TransportResult<serde_json::Value> {
                Ok(json!({"id":1,"is_bot":true,"username":"fixture_bot"}))
            }
            fn updates(&mut self, _: i64, _: u64) -> TransportResult<Vec<serde_json::Value>> {
                unreachable!()
            }
            fn send_text(
                &mut self,
                chat: i64,
                text: &str,
                _: Option<i64>,
            ) -> TransportResult<serde_json::Value> {
                self.sent.push(text.into());
                Ok(json!({"message_id":50+self.sent.len(),"chat":{"id":chat}}))
            }
            fn download(&mut self, _: &str) -> TransportResult<Vec<u8>> {
                unreachable!()
            }
            fn send_document(
                &mut self,
                _: i64,
                _: &str,
                _: Vec<u8>,
                _: &str,
            ) -> TransportResult<serde_json::Value> {
                unreachable!()
            }
        }
        struct FeederFixture {
            target: TerminalId,
            accepted: Option<Frame>,
        }
        impl Feeder for FeederFixture {
            fn live(&mut self) -> Result<Vec<TerminalId>> {
                Ok(vec![self.target.clone()])
            }
            fn submit(&mut self, frame: &Frame) -> Result<Submission> {
                self.accepted = Some(frame.clone());
                Ok(Submission::Accepted(json!({"submitted":true})))
            }
        }
        let Fixture {
            mut store,
            journal,
            routes,
            config,
            mut registration,
            plan,
            frame,
        } = Fixture::new();
        registration.options.telegram_delivery = true;
        store.start(&plan, 0).unwrap();
        let mut engine = Engine::new(journal, routes, "fixture", 1).unwrap();
        engine.streams = Some(store);
        engine
            .configure_hooks(vec![registration.clone()], &config.hook_bindings)
            .unwrap();
        let mut api = ApiFixture { sent: vec![] };
        engine.start(&mut api).unwrap();
        engine.admit_updates(vec![json!({"update_id":4,"message":{"message_id":3,"chat":{"id":2,"type":"private"},"from":{"id":2,"is_bot":false},"text":"request"}})],1.0).unwrap();
        let mut feeder = FeederFixture {
            target: frame.terminal.clone(),
            accepted: None,
        };
        engine
            .stage(
                &mut api,
                &mut feeder,
                &mut PrivateFiles {
                    directory: crate::test_support::path("/tmp/never-created-stream-fixture")
                        .into(),
                },
                2.0,
            )
            .unwrap();
        engine
            .feed_one(&mut feeder, &frame.terminal, || 3.0)
            .unwrap();
        let accepted = feeder.accepted.unwrap();
        let hash = format!(
            "{:x}",
            Sha256::digest(
                super::super::telegram_feeder::encode_prompt(&accepted)
                    .unwrap()
                    .0
                    .as_bytes()
            )
        );
        for (id, kind, text) in [
            ("prompt", EventKind::UserPromptSubmit, ""),
            ("answer", EventKind::Stop, "public assistant response"),
        ] {
            engine.journal.admit("hooks","fixture",id,&json!({"registration_id":"hook","scope":registration.scope,"event":kind,"transcript":null,"stream_turn_id":"turn","stream_prompt_sha256":hash,"final_message":text,"effects":if kind==EventKind::Stop {vec!["queue_final_reply"]}else{vec![]}}),4.0).unwrap();
        }
        engine
            .streams
            .as_mut()
            .unwrap()
            .tick(
                &mut engine.journal,
                &engine.routes,
                &config,
                &[(
                    crate::test_support::path("/tmp/unused").into(),
                    registration.clone(),
                )],
                5.0,
            )
            .unwrap();
        let hooks = engine
            .journal
            .events_page("hooks", "fixture", &["admitted"], 0, 10)
            .unwrap();
        for event in hooks {
            super::super::hook_effects::expand_many(
                &mut engine.journal,
                &engine.routes,
                &registration,
                &config.hook_bindings,
                &event,
                5.0,
            )
            .unwrap();
        }
        assert_eq!(
            engine
                .journal
                .rows()
                .unwrap()
                .iter()
                .filter(|r| r.direction == "outbound")
                .count(),
            1
        );
        assert!(engine.send_one(&mut api, 2, || 6.0).unwrap());
        assert_eq!(api.sent, vec!["public assistant response"]);
        assert!(!engine.send_one(&mut api, 2, || 7.0).unwrap());
    }
    #[test]
    fn restart_preserves_cursor_and_stop_epochs_without_replay() {
        use std::{
            fs,
            os::unix::fs::PermissionsExt,
            time::{SystemTime, UNIX_EPOCH},
        };
        let directory = std::env::temp_dir().join(format!(
            "daedalus-stream-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir(&directory).unwrap();
        fs::set_permissions(&directory, fs::Permissions::from_mode(0o700)).unwrap();
        let mut f = Fixture::new();
        f.store = Store::open(&directory).unwrap();
        f.start();
        f.accept();
        f.prompt("prompt", "turn");
        f.event("answer", EventKind::Stop, Some("turn"), None, "answer");
        f.tick();
        f.store = Store::open(&directory).unwrap();
        f.tick();
        assert_eq!(f.rows().len(), 1);
        let row = f.rows().remove(0);
        f.store
            .authorize_delivery(&row, &f.routes, &f.journal)
            .unwrap();
        f.store.control(&f.caller(), Setting::Off).unwrap();
        f.store = Store::open(&directory).unwrap();
        assert!(
            f.store
                .authorize_delivery(&row, &f.routes, &f.journal)
                .is_err()
        );
        drop(f);
        fs::remove_dir_all(directory).unwrap();
    }
}
