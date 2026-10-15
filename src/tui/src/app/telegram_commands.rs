//! Authorized Telegram controls. Names are display text; targets use exact IDs.
//! Confirmations are actor/chat/revision scoped, durable, and consumed before effects.
use crate::core::{
    identity::TerminalId,
    routes::{Permission, Registry, Scope, Terminal},
};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
type Result<T> = std::result::Result<T, String>;
#[derive(Clone, Debug)]
pub struct Caller {
    pub scope: Scope,
    pub actor: String,
    pub human: bool,
    pub forwarded: bool,
    pub bot_username: String,
    pub event_id: String,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Command {
    Menu,
    Status,
    Windows,
    Use(Option<String>),
    Pin(bool),
    Capture,
    Interrupt,
    Close,
    Stream {
        mode: super::telegram_stream::Mode,
        setting: super::telegram_stream::Setting,
    },
    AgentRename {
        target: String,
        base: String,
    },
    AgentRenameStatus(String),
    SessionRename(String),
    SessionRenameStatus(String),
    New(super::telegram_launch::Selection),
    Say(String),
    Listen(bool),
    Harness(Action),
    Keys(Vec<String>),
    Busy,
    Queue,
    Sessions,
    Goal(Action),
    Checkpoint(Action),
    Unavailable(String),
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum Action {
    NativeSlash { command: String, arguments: String },
    Select,
    Pin(bool),
    Capture,
    Interrupt,
    Close,
    AgentRename(Box<super::agent_identity::Plan>),
    StreamStart(Box<super::telegram_stream::Plan>),
    SessionRename(Box<super::telegram_rename::Plan>),
    New(Box<super::telegram_launch::Reviewed>),
    Keys(Vec<String>),
    Busy,
    GoalStatus,
    GoalArm(String),
    GoalPause,
    GoalResume,
    GoalComplete,
    CheckpointList,
    CheckpointSave(String),
    CheckpointRemove(String),
    CheckpointResume(String),
    CheckpointFork(String),
}
impl Action {
    fn mutates(&self) -> bool {
        !matches!(
            self,
            Self::Capture | Self::Busy | Self::GoalStatus | Self::CheckpointList
        )
    }
}
/// None means explicitly addressed to another bot, never an agent prompt.
pub fn parse(text: &str, username: &str) -> Result<Option<Command>> {
    if text.len() > 8192 || text.contains('\0') || !text.starts_with('/') {
        return Err("Invalid control command".into());
    }
    let mut parts = text.splitn(2, char::is_whitespace);
    let head = parts.next().unwrap_or_default();
    let rest = parts.next().unwrap_or_default().trim();
    let (name, suffix) = head
        .split_once('@')
        .map_or((head, None), |(name, suffix)| (name, Some(suffix)));
    if suffix.is_some_and(|suffix| !suffix.eq_ignore_ascii_case(username)) {
        return Ok(None);
    }
    let bare = name.strip_prefix('/').unwrap_or_default();
    if bare.is_empty()
        || bare.len() > 32
        || !bare
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
    {
        return Err("Invalid command name".into());
    }
    let no_args = || {
        if rest.is_empty() {
            Ok(())
        } else {
            Err("Unexpected control arguments".to_owned())
        }
    };
    Ok(Some(match name {
        "/menu" | "/panel" | "/help" => {
            no_args()?;
            Command::Menu
        }
        "/status" | "/where" => {
            no_args()?;
            Command::Status
        }
        "/windows" | "/jobs" => {
            no_args()?;
            Command::Windows
        }
        "/stream" | "/reason" | "/verbose" => Command::Stream {
            mode: if name == "/stream" {
                super::telegram_stream::Mode::Text
            } else {
                super::telegram_stream::Mode::Work
            },
            setting: super::telegram_stream::setting(rest)?,
        },
        "/mode" => Command::Unavailable("mode".into()),
        "/license" | "/lic" => Command::Unavailable("license".into()),
        "/mirror" | "/console" => Command::Unavailable("mirror".into()),
        "/rename" => {
            let parts = rest.split_whitespace().collect::<Vec<_>>();
            if parts.len() != 2
                || parts.iter().any(|p| {
                    p.len() > 128 || !p.chars().all(|c| c.is_alphanumeric() || "_-".contains(c))
                })
            {
                return Err("Use /rename EXACT_AGENT_NAME NEW_BASE on the selected route".into());
            }
            Command::AgentRename {
                target: parts[0].into(),
                base: parts[1].into(),
            }
        }
        "/rename_status" => {
            if rest.len() != 67
                || !rest.starts_with("ar:")
                || !rest[3..].bytes().all(|b| b.is_ascii_hexdigit())
            {
                return Err("Use /rename_status ar:OPERATION_ID".into());
            }
            Command::AgentRenameStatus(rest.into())
        }
        "/session_rename" => Command::SessionRename(super::telegram_rename::name(rest)?),
        "/session_rename_status" => {
            Command::SessionRenameStatus(super::telegram_rename::operation(rest)?)
        }
        "/new" => Command::New(super::telegram_launch::parse(rest)?),
        "/kill" => {
            no_args()?;
            Command::Close
        }
        "/say" => {
            if rest.is_empty() {
                return Err("Use /say <text> for the current route".into());
            }
            Command::Say(rest.into())
        }
        "/sessions" => {
            no_args()?;
            Command::Sessions
        }
        "/busy" => {
            no_args()?;
            Command::Busy
        }
        "/pending" => {
            no_args()?;
            Command::Queue
        }
        "/stop" | "/esc" => {
            no_args()?;
            Command::Keys(vec!["Escape".into()])
        }
        "/enter" => {
            no_args()?;
            Command::Keys(vec!["Enter".into()])
        }
        "/keys" => {
            let keys: Vec<String> = rest.split_whitespace().map(str::to_owned).collect();
            validate_keys(&keys)?;
            Command::Keys(keys)
        }
        "/clear" | "/compact" | "/context" | "/cost" | "/agents" | "/mcp" | "/review"
        | "/secreview" | "/export" | "/config" | "/memory" | "/cc" | "/model" | "/effort"
        | "/plan" | "/security-review" => {
            let command = name.trim_start_matches('/');
            // Syntax validation only. The exact running provider/version is checked at execution.
            crate::adapters::harness_commands::plan("claude", "2.1.296", command, rest)
                .map_err(|e| e.to_string())?;
            Command::Harness(Action::NativeSlash {
                command: command.into(),
                arguments: rest.into(),
            })
        }
        "/quiet" | "/listen" => {
            no_args()?;
            Command::Listen(name == "/listen")
        }
        "/resume" => Command::Checkpoint(Action::CheckpointResume(checkpoint_key(rest)?)),
        "/use" => Command::Use(if rest.is_empty() {
            None
        } else {
            Some(target_key(rest)?)
        }),
        "/pin" => {
            no_args()?;
            Command::Pin(true)
        }
        "/unpin" => {
            no_args()?;
            Command::Pin(false)
        }
        "/capture" => {
            no_args()?;
            Command::Capture
        }
        "/interrupt" | "/ctrlc" => {
            no_args()?;
            Command::Interrupt
        }
        "/goal" => {
            let (verb, value) = rest.split_once(char::is_whitespace).unwrap_or((rest, ""));
            let value = value.trim();
            Command::Goal(match verb {
                "" | "status" if value.is_empty() => Action::GoalStatus,
                "pause" if value.is_empty() => Action::GoalPause,
                "resume" if value.is_empty() => Action::GoalResume,
                "complete" if value.is_empty() => Action::GoalComplete,
                "arm" if !value.is_empty() && value.len() <= 4096 => Action::GoalArm(value.into()),
                _ => return Err("Use /goal status|arm <directive>|pause|resume|complete".into()),
            })
        }
        "/checkpoint" | "/checkpoints" => {
            let (verb, value) = rest.split_once(char::is_whitespace).unwrap_or((rest, ""));
            let value = value.trim();
            Command::Checkpoint(match verb {
                "" | "list" if value.is_empty() => Action::CheckpointList,
                "save"
                    if !value.is_empty()
                        && value.chars().count() <= 100
                        && !value.chars().any(char::is_control) =>
                {
                    Action::CheckpointSave(value.into())
                }
                "remove" => Action::CheckpointRemove(checkpoint_key(value)?),
                "resume" => Action::CheckpointResume(checkpoint_key(value)?),
                "fork" => Action::CheckpointFork(checkpoint_key(value)?),
                _ => {
                    return Err(
                        "Use /checkpoint list|save <name>|remove|resume|fork <checkpoint key>"
                            .into(),
                    );
                }
            })
        }
        _ => return Err("Unknown command; use /menu".into()),
    }))
}
fn target_key(value: &str) -> Result<String> {
    if value.len() == 66
        && value.starts_with("t:")
        && value[2..].bytes().all(|b| b.is_ascii_hexdigit())
    {
        Ok(value.into())
    } else {
        Err("Use the exact terminal key from /windows or choose its button".into())
    }
}
fn checkpoint_key(value: &str) -> Result<String> {
    if value.len() == 75
        && value.starts_with("checkpoint:")
        && value[11..].bytes().all(|b| b.is_ascii_hexdigit())
    {
        Ok(value.into())
    } else {
        Err("Use the exact checkpoint key from /checkpoint list".into())
    }
}
pub fn terminal_key(id: &TerminalId) -> String {
    format!(
        "t:{:x}",
        Sha256::digest(serde_json::to_vec(id).expect("terminal identity"))
    )
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Target {
    pub terminal: TerminalId,
    pub conversation: Option<String>,
}
impl From<&Terminal> for Target {
    fn from(terminal: &Terminal) -> Self {
        Self {
            terminal: terminal.identity.clone(),
            conversation: terminal.conversation.clone(),
        }
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Button {
    pub text: String,
    pub callback_data: String,
}
#[derive(Clone, Debug, Serialize, Deserialize, Default)]
pub struct Reply {
    pub text: String,
    pub buttons: Vec<Vec<Button>>,
}
impl Reply {
    fn text(text: impl Into<String>) -> Self {
        Self {
            text: bounded_text(&text.into(), 4000),
            buttons: vec![],
        }
    }
}
fn bounded_text(text: &str, limit: usize) -> String {
    let mut count = 0;
    text.chars()
        .filter(|c| !c.is_control() || *c == '\n' || *c == '\t')
        .take_while(|c| {
            count += c.len_utf16();
            count <= limit
        })
        .collect()
}
fn validate_keys(keys: &[String]) -> Result<()> {
    if keys.is_empty()
        || keys.len() > 64
        || keys.iter().any(|key| {
            key.is_empty()
                || key.len() > 40
                || key.starts_with('-')
                || !key
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"-+_".contains(&b))
        })
    {
        return Err("Use 1–64 terminal key names (for example Escape, Enter, C-c)".into());
    }
    Ok(())
}
pub trait Operations {
    /// Read-only fresh evidence. Implementations must fail rather than substitute stale inventory.
    fn inventory(&mut self) -> Result<Vec<Terminal>>;
    /// Must revalidate the target again immediately before terminal/session effects.
    fn execute(&mut self, action: &Action, target: &Target) -> Result<String>;
    fn prepare_stream(
        &mut self,
        _caller: &Caller,
        _target: &Target,
        _revision: i64,
        _mode: super::telegram_stream::Mode,
    ) -> Result<super::telegram_stream::Plan> {
        Err("Streaming unavailable".into())
    }
    fn stream_control(
        &mut self,
        _caller: &Caller,
        _setting: super::telegram_stream::Setting,
    ) -> Result<String> {
        Err("Streaming unavailable".into())
    }
    fn prepare_agent_rename(
        &mut self,
        _caller: &Caller,
        _target: &Target,
        _name: &str,
        _base: &str,
    ) -> Result<super::agent_identity::Plan> {
        Err("Optional Armonia identity rename unavailable".into())
    }
    fn agent_rename_status(&mut self, _caller: &Caller, _operation: &str) -> Result<String> {
        Err("Optional Armonia identity status unavailable".into())
    }
    fn prepare_session_rename(
        &mut self,
        _caller: &Caller,
        _target: &Target,
        _name: &str,
    ) -> Result<super::telegram_rename::Plan> {
        Err("Verified session rename unavailable".into())
    }
    fn session_rename_status(&mut self, _caller: &Caller, _operation: &str) -> Result<String> {
        Err("Session rename status unavailable".into())
    }
    fn prepare_new(
        &mut self,
        _selection: &super::telegram_launch::Selection,
        _target: &Target,
        _now: u64,
    ) -> Result<super::telegram_launch::Reviewed> {
        Err("Reviewed hooked launch is unavailable".into())
    }
    fn session_scopes(&mut self, _caller: &Caller) -> Result<Vec<Scope>> {
        Err("Session bindings unavailable".into())
    }
    /// Aggregate delivery states only, restricted to authenticated chat and exact route.
    fn pending(&mut self, _caller: &Caller, _target: &Target) -> Result<String> {
        Err("Queue observations unavailable".into())
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
struct Claim {
    scope: Scope,
    actor: String,
    target: Target,
    revision: i64,
    action: Action,
}
const SCHEMA: &str = "PRAGMA synchronous=FULL; CREATE TABLE IF NOT EXISTS telegram_controls(token TEXT PRIMARY KEY,issue_key TEXT NOT NULL UNIQUE,claim TEXT NOT NULL,expires_ms INTEGER NOT NULL,state TEXT NOT NULL,result TEXT); CREATE INDEX IF NOT EXISTS telegram_controls_expiry ON telegram_controls(expires_ms,state);";
pub struct CallbackStore {
    con: Connection,
}
impl CallbackStore {
    /// Caller supplies a private owned connection. No ambient file paths are opened.
    pub fn from_connection(con: Connection) -> Result<Self> {
        con.busy_timeout(std::time::Duration::from_secs(5))
            .map_err(|_| "Callback store unavailable")?;
        con.execute_batch(SCHEMA)
            .map_err(|_| "Cannot initialize callback store")?;
        Ok(Self { con })
    }
    pub fn memory() -> Result<Self> {
        Self::from_connection(
            Connection::open_in_memory().map_err(|_| "Cannot initialize callback store")?,
        )
    }
    fn issue(&mut self, caller: &Caller, claim: Claim, now: u64) -> Result<String> {
        let expires = i64::try_from(
            now.checked_add(5 * 60 * 1000)
                .ok_or("Invalid callback time")?,
        )
        .map_err(|_| "Invalid callback time")?;
        let encoded = serde_json::to_string(&claim).map_err(|_| "Cannot encode confirmation")?;
        let issue_key = format!(
            "{:x}",
            Sha256::digest(
                serde_json::to_vec(&(&caller.scope, &caller.actor, &caller.event_id, &encoded))
                    .map_err(|_| "Cannot encode confirmation")?
            )
        );
        let tx = self
            .con
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|_| "Callback store unavailable")?;
        if let Some(token) = tx
            .query_row(
                "SELECT token FROM telegram_controls WHERE issue_key=?",
                [&issue_key],
                |r| r.get::<_, String>(0),
            )
            .optional()
            .map_err(|_| "Cannot read confirmation")?
        {
            return Ok(token);
        }
        let token: String = tx
            .query_row("SELECT 'dc:' || lower(hex(randomblob(16)))", [], |r| {
                r.get(0)
            })
            .map_err(|_| "Cannot allocate confirmation")?;
        tx.execute("INSERT INTO telegram_controls(token,issue_key,claim,expires_ms,state) VALUES(?,?,?,?,'pending')",params![token,issue_key,encoded,expires]).map_err(|_|"Cannot persist confirmation")?;
        tx.commit().map_err(|_| "Cannot persist confirmation")?;
        Ok(token)
    }
    fn peek(&self, caller: &Caller, token: &str, now: u64) -> Result<Claim> {
        if token.len() != 35
            || !token.starts_with("dc:")
            || !token[3..].bytes().all(|b| b.is_ascii_hexdigit())
        {
            return Err("Invalid confirmation token".into());
        }
        let row: Option<(String, i64, String)> = self
            .con
            .query_row(
                "SELECT claim,expires_ms,state FROM telegram_controls WHERE token=?",
                [token],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()
            .map_err(|_| "Cannot read confirmation")?;
        let (encoded, expires, state) = row.ok_or("Unknown confirmation; open a new menu")?;
        let claim: Claim =
            serde_json::from_str(&encoded).map_err(|_| "Invalid saved confirmation")?;
        if claim.scope != caller.scope || claim.actor != caller.actor {
            return Err("Confirmation belongs to another actor or chat".into());
        }
        if state != "pending" {
            return Err("Confirmation already consumed; inspect status before retrying".into());
        }
        if i64::try_from(now).map_err(|_| "Invalid callback time")? >= expires {
            return Err("Confirmation expired; open a new menu".into());
        }
        Ok(claim)
    }
    fn consume(&mut self, caller: &Caller, token: &str, claim: &Claim, now: u64) -> Result<()> {
        let actual = self.peek(caller, token, now)?;
        let encoded = serde_json::to_string(&actual).map_err(|_| "Invalid confirmation")?;
        if encoded != serde_json::to_string(claim).map_err(|_| "Invalid confirmation")? {
            return Err("Confirmation changed".into());
        }
        let changed=self.con.execute("UPDATE telegram_controls SET state='consumed_unknown' WHERE token=? AND state='pending' AND claim=? AND expires_ms>?",params![token,encoded,i64::try_from(now).map_err(|_|"Invalid callback time")?]).map_err(|_|"Cannot consume confirmation")?;
        if changed != 1 {
            return Err("Confirmation already consumed or expired".into());
        }
        Ok(())
    }
    fn finish(&mut self, token: &str, success: bool) -> Result<()> {
        self.con
            .execute(
                "UPDATE telegram_controls SET state=? WHERE token=? AND state='consumed_unknown'",
                params![
                    if success {
                        "completed"
                    } else {
                        "failed_or_unknown"
                    },
                    token
                ],
            )
            .map_err(|_| "Cannot persist control outcome")?;
        Ok(())
    }
}
pub struct Dispatcher {
    pub callbacks: CallbackStore,
}
impl Dispatcher {
    fn authorize(caller: &Caller, routes: &Registry, permission: Permission) -> Result<()> {
        if !caller.human
            || caller.forwarded
            || caller.actor.is_empty()
            || !caller.actor.bytes().all(|b| b.is_ascii_digit())
            || caller.event_id.is_empty()
            || caller.event_id.len() > 256
            || caller.event_id.chars().any(char::is_control)
        {
            return Err("Controls require an authenticated human sender and direct message".into());
        }
        routes
            .authorize(&caller.scope, &caller.actor, Permission::Read)
            .map_err(|_| "Control is not authorized")?;
        routes
            .authorize(&caller.scope, &caller.actor, permission)
            .map_err(|_| "Control is not authorized".to_owned())
    }
    fn inventory(operations: &mut impl Operations) -> Result<Vec<Terminal>> {
        let terminals = operations.inventory()?;
        if terminals.len() > 10000 {
            return Err("Inventory exceeds control limit".into());
        }
        let mut seen = BTreeSet::new();
        for terminal in &terminals {
            terminal.identity.validate().map_err(str::to_owned)?;
            if !seen.insert(terminal_key(&terminal.identity)) {
                return Err("Ambiguous terminal inventory".into());
            }
        }
        Ok(terminals.into_iter().filter(|t| t.available).collect())
    }
    fn offer(
        &mut self,
        caller: &Caller,
        target: Target,
        revision: i64,
        action: Action,
        now: u64,
        label: &str,
    ) -> Result<Button> {
        let token = self.callbacks.issue(
            caller,
            Claim {
                scope: caller.scope.clone(),
                actor: caller.actor.clone(),
                target,
                revision,
                action,
            },
            now,
        )?;
        Ok(Button {
            text: label.chars().filter(|c| !c.is_control()).take(64).collect(),
            callback_data: token,
        })
    }
    pub fn command(
        &mut self,
        caller: &Caller,
        command: Command,
        routes: &mut Registry,
        operations: &mut impl Operations,
        now: u64,
    ) -> Result<Reply> {
        let permission = match &command {
            Command::Use(_)
            | Command::Pin(_)
            | Command::Interrupt
            | Command::Keys(_)
            | Command::Stream { .. }
            | Command::AgentRename { .. }
            | Command::AgentRenameStatus(_)
            | Command::SessionRename(_)
            | Command::Close
            | Command::New(_) => Permission::Manage,
            Command::Harness(_) | Command::Listen(_) => Permission::Manage,
            Command::Goal(a) | Command::Checkpoint(a) if a.mutates() => Permission::Manage,
            Command::Say(_) => Permission::Send,
            _ => Permission::Read,
        };
        Self::authorize(caller, routes, permission)?;
        if let Command::Stream { setting, .. } = &command {
            super::telegram_stream::authorize_caller(caller, routes)?;
            if *setting != super::telegram_stream::Setting::On {
                return operations.stream_control(caller, *setting).map(Reply::text);
            }
        }
        if let Command::AgentRenameStatus(operation) = &command {
            return operations
                .agent_rename_status(caller, operation)
                .map(Reply::text);
        }
        if let Command::SessionRenameStatus(operation) = &command {
            return operations
                .session_rename_status(caller, operation)
                .map(Reply::text);
        }
        if matches!(command, Command::Say(_)) {
            return Err("Explicit prompts require journal feeder admission".into());
        }
        if let Command::Keys(keys) = &command {
            validate_keys(keys)?;
        }
        if let Command::Listen(listen) = command {
            let current = routes
                .input_policy(&caller.scope)
                .map_err(|e| e.to_string())?;
            let policy = routes
                .set_input_policy(&caller.scope, &caller.actor, current.revision, listen)
                .map_err(|e| e.to_string())?;
            return Ok(Reply::text(format!(
                "{} This chat only; sender permissions still apply. Direct messages are unchanged. Policy revision {}.",
                if listen {
                    "Group listening enabled."
                } else {
                    "Mention/reply-only enabled."
                },
                policy.revision
            )));
        }
        if let Command::Unavailable(name) = command {
            return Ok(Reply::text(match name.as_str() {
                "mode" => {
                    "Remote permission-mode cycling is unsupported. Use the harness’s native permission controls. /plan remains available for audited Claude versions. No keys were sent."
                }
                "license" => {
                    "Deployment-specific license administration was removed. Use configured native harness profiles and TUI account sign-in. No provider request or agent action was performed."
                }
                "mirror" => {
                    "Raw terminal console export was removed. Use /stream for assistant text or /reason for text and tool names at verified hook boundaries (DM only). No terminal output was exported."
                }
                _ => "This operation is unavailable; no action was performed.",
            }));
        }
        if command == Command::Menu {
            return Ok(Reply::text(
                "Daedalus controls\n/status — current route\n/quiet /listen — set this group’s input policy immediately (Manage required)\n/resume <checkpoint key> — confirm saved-session resume\n/windows — exact terminal keys\n/use <key> — select a route\n/pin /unpin — route policy\n/capture — visible terminal output\n/interrupt — confirm Ctrl-C\n/kill — confirm terminal closure\n/rename EXACT_AGENT_NAME NEW_BASE — optional Armonia identity rename\n/rename_status OP — reconcile identity receipt\n/session_rename NAME — confirm selected tmux session rename\n/session_rename_status OP — verify without replay\n/new claude|pi|grok [split] — review hooked session launch\n/say <text> — queue text for the current route\n/stop /esc — confirm Escape\n/enter /keys <names> — confirm key input\n/stream on|off|status — assistant text at hook boundaries (DM only)\n/reason /verbose — same controls, also tool names; no hidden reasoning\n/busy — verified hook readiness\n/clear /compact /context /cost /agents /mcp /review /secreview /export /config /memory /model /effort /plan /cc — confirmed commands for audited Claude versions\n/pending — this chat’s route queue counts\n/sessions — authorized chat-to-terminal bindings\n/goal status|arm <directive>|pause|resume|complete\n/checkpoint list|save <name>|remove|resume|fork <key>\nMutations require a one-use confirmation button.",
            ));
        }
        let route = routes
            .inspect(&caller.scope, &caller.actor)
            .map_err(|e| e.to_string())?;
        if command == Command::Status {
            return Ok(Reply::text(match route {
                Some(route) => format!(
                    "Route: {}\nPinned: {}\nRevision: {}\nTerminal identity recorded; this is not proof the agent is ready.",
                    terminal_key(&route.terminal),
                    route.pinned,
                    route.revision
                ),
                None => "No route selected. Use /windows then /use.".into(),
            }));
        }
        let live = Self::inventory(operations)?;
        if command == Command::Sessions {
            let scopes = operations.session_scopes(caller)?;
            if scopes.len() > 10000 {
                return Err("Session binding limit exceeded".into());
            }
            let mut rows = Vec::new();
            let mut seen = BTreeSet::new();
            for scope in scopes {
                if scope.instance != caller.scope.instance
                    || scope.bot != caller.scope.bot
                    || !seen.insert(scope.chat.clone())
                {
                    continue;
                }
                // Authorization is checked separately for every chat, not inherited from the requesting chat.
                if routes
                    .authorize(&scope, &caller.actor, Permission::Read)
                    .is_err()
                {
                    continue;
                }
                if let Some(route) = routes
                    .inspect(&scope, &caller.actor)
                    .map_err(|_| "Session binding unavailable")?
                {
                    let state = if live
                        .iter()
                        .any(|terminal| terminal.identity == route.terminal)
                    {
                        "live terminal"
                    } else {
                        "unavailable terminal"
                    };
                    rows.push(format!(
                        "Chat {} → {} ({state}; revision {})",
                        scope.chat,
                        terminal_key(&route.terminal),
                        route.revision
                    ));
                }
            }
            return Ok(Reply::text(bounded_text(
                &format!(
                    "Authorized session bindings (terminal presence does not prove agent readiness):\n{}",
                    if rows.is_empty() {
                        "No authorized bindings.".into()
                    } else {
                        rows.join("\n")
                    }
                ),
                4000,
            )));
        }
        let revision = route.as_ref().map_or(0, |r| r.revision);
        if matches!(command, Command::Windows | Command::Use(None)) {
            let mut reply = Reply::text(if live.is_empty() {
                "No verified live terminals."
            } else {
                "Live terminals (first 20); keys identify exact generations:"
            });
            let can_manage = routes
                .authorize(&caller.scope, &caller.actor, Permission::Manage)
                .is_ok();
            for terminal in live.iter().take(20) {
                let key = terminal_key(&terminal.identity);
                reply.text.push_str(&format!(
                    "\n{} {}\n{}",
                    terminal.harness,
                    terminal
                        .label
                        .chars()
                        .filter(|c| !c.is_control())
                        .take(60)
                        .collect::<String>(),
                    key
                ));
                if can_manage {
                    reply.buttons.push(vec![self.offer(
                        caller,
                        terminal.into(),
                        revision,
                        Action::Select,
                        now,
                        &format!("Use {}", terminal.label),
                    )?]);
                }
            }
            reply.text = bounded_text(&reply.text, 4000);
            return Ok(reply);
        }
        let (target, action) = match command {
            Command::Use(Some(key)) => {
                let terminal = live
                    .iter()
                    .find(|t| terminal_key(&t.identity) == key)
                    .ok_or("Exact terminal is unavailable")?;
                (Target::from(terminal), Action::Select)
            }
            other => {
                let route = route.ok_or("Select a route with /use first")?;
                let terminal = live
                    .iter()
                    .find(|t| t.identity == route.terminal)
                    .ok_or("Selected terminal is unavailable")?;
                let action = match other {
                    Command::Pin(pin) => Action::Pin(pin),
                    Command::Capture => Action::Capture,
                    Command::Interrupt => Action::Interrupt,
                    Command::Close => Action::Close,
                    Command::AgentRename { target: name, base } => {
                        Action::AgentRename(Box::new(operations.prepare_agent_rename(
                            caller,
                            &Target::from(terminal),
                            &name,
                            &base,
                        )?))
                    }
                    Command::Stream {
                        mode,
                        setting: super::telegram_stream::Setting::On,
                    } => Action::StreamStart(Box::new(operations.prepare_stream(
                        caller,
                        &Target::from(terminal),
                        revision,
                        mode,
                    )?)),
                    Command::SessionRename(name) => {
                        Action::SessionRename(Box::new(operations.prepare_session_rename(
                            caller,
                            &Target::from(terminal),
                            &name,
                        )?))
                    }
                    Command::New(selection) => Action::New(Box::new(operations.prepare_new(
                        &selection,
                        &Target::from(terminal),
                        now,
                    )?)),
                    Command::Keys(keys) => Action::Keys(keys),
                    Command::Busy => Action::Busy,
                    Command::Queue => {
                        return operations
                            .pending(caller, &Target::from(terminal))
                            .map(Reply::text);
                    }
                    Command::Harness(action)
                    | Command::Goal(action)
                    | Command::Checkpoint(action) => action,
                    _ => return Err("Invalid control action".into()),
                };
                (Target::from(terminal), action)
            }
        };
        if !action.mutates() {
            return operations.execute(&action, &target).map(Reply::text);
        }
        let description = match &action {
            Action::New(reviewed) => reviewed.description()?,
            Action::StreamStart(plan) => plan.description(),
            Action::SessionRename(plan) => plan.description(),
            Action::AgentRename(plan) => format!(
                "Rename optional Armonia agent identity to {}. Its permanent ID and existing history are retained; provider conversations are unchanged. Operation: {}",
                plan.display, plan.operation
            ),
            _ => serde_json::to_string(&action).map_err(|_| "Cannot describe confirmation")?,
        };
        let mut description = description;
        if let Action::NativeSlash { command, arguments } = &action {
            let plan =
                crate::adapters::harness_commands::plan("claude", "2.1.296", command, arguments)
                    .map_err(|e| e.to_string())?;
            description.push_str(&format!("\nRequires verified Claude Code 2.1.296. May invoke model work: {}. Changes context: {}. May need native interaction: {}. Submission does not verify completion or changed settings.", plan.may_invoke_model, plan.changes_context, plan.may_require_interaction));
        }
        let text = format!(
            "Action: {}\nTerminal: {}\nConversation: {}\nRoute revision: {}\nConfirm once within five minutes.",
            description.chars().take(1600).collect::<String>(),
            terminal_key(&target.terminal),
            target.conversation.as_deref().unwrap_or("not reported"),
            revision
        );
        let button = self.offer(caller, target, revision, action, now, "Confirm this action")?;
        Ok(Reply {
            text: bounded_text(&text, 4000),
            buttons: vec![vec![button]],
        })
    }
    pub fn callback(
        &mut self,
        caller: &Caller,
        token: &str,
        routes: &mut Registry,
        operations: &mut impl Operations,
        now: u64,
    ) -> Result<Reply> {
        Self::authorize(caller, routes, Permission::Manage)?;
        let claim = self.callbacks.peek(caller, token, now)?;
        if let Action::StreamStart(plan) = &claim.action {
            super::telegram_stream::authorize_caller(caller, routes)?;
            if plan.binding.route_scope != caller.scope
                || plan.binding.actor != caller.actor
                || plan.binding.route_revision != claim.revision
                || plan.binding.goal_scope.terminal != claim.target.terminal
                || Some(&plan.binding.goal_scope.conversation) != claim.target.conversation.as_ref()
            {
                return Err("Stream review identity mismatch".into());
            }
        }
        if let Action::AgentRename(plan) = &claim.action
            && (plan.scope != caller.scope || plan.actor != caller.actor)
        {
            return Err("Agent rename belongs to another actor or chat".into());
        }
        if let Action::SessionRename(plan) = &claim.action
            && (plan.scope != caller.scope || plan.actor != caller.actor)
        {
            return Err("Rename review belongs to another actor or chat".into());
        }
        let current = routes
            .inspect(&caller.scope, &caller.actor)
            .map_err(|e| e.to_string())?;
        if current.as_ref().map_or(0, |r| r.revision) != claim.revision
            || (claim.action != Action::Select
                && current
                    .as_ref()
                    .is_none_or(|r| r.terminal != claim.target.terminal))
        {
            return Err("Route changed; open a new confirmation".into());
        }
        let live = Self::inventory(operations)?;
        let terminal = live
            .iter()
            .find(|t| t.identity == claim.target.terminal)
            .ok_or("Exact terminal is unavailable")?;
        if terminal.conversation != claim.target.conversation {
            return Err("Conversation changed; open a new confirmation".into());
        }
        // Publish the same complete fresh endpoint observation used for this decision.
        // A newly discovered target may not exist in the routing database yet.
        if claim.action == Action::Select {
            let endpoint_rows: Vec<_> = live
                .iter()
                .filter(|row| {
                    row.identity.backend == claim.target.terminal.backend
                        && row.identity.endpoint == claim.target.terminal.endpoint
                })
                .cloned()
                .collect();
            routes
                .inventory(
                    claim.target.terminal.backend.clone(),
                    &claim.target.terminal.endpoint,
                    &endpoint_rows,
                    true,
                    true,
                )
                .map_err(|_| "Cannot record fresh terminal inventory")?;
        }
        self.callbacks.consume(caller, token, &claim, now)?;
        let result = match claim.action {
            Action::Select => routes
                .select(
                    &caller.scope,
                    &caller.actor,
                    &claim.target.terminal,
                    &live.iter().map(|t| t.identity.clone()).collect::<Vec<_>>(),
                    claim.revision,
                    true,
                    false,
                )
                .map(|_| "Exact terminal route selected".to_owned())
                .map_err(|e| e.to_string()),
            Action::Pin(pin) => routes
                .set_pinned(&caller.scope, &caller.actor, claim.revision, pin)
                .map(|_| {
                    if pin {
                        "Route pinned"
                    } else {
                        "Route unpinned"
                    }
                    .to_owned()
                })
                .map_err(|e| e.to_string()),
            action => operations.execute(&action, &claim.target),
        };
        self.callbacks.finish(token, result.is_ok())?;
        result.map(Reply::text)
    }
}

fn queue_counts(
    con: &Connection,
    scope: &Scope,
    destination: &str,
    conversation: Option<&str>,
) -> Result<String> {
    let mut statement = con.prepare("SELECT state,count(*) FROM delivery WHERE direction='inbound' AND destination=?1 AND json_valid(payload) AND json_extract(payload,'$.scope.instance')=?2 AND json_extract(payload,'$.scope.bot')=?3 AND json_extract(payload,'$.scope.chat')=?4 AND json_extract(payload,'$.conversation') IS ?5 AND state IN ('pending','retryable','intent','unknown','blocked') GROUP BY state ORDER BY state").map_err(|_| "Queue observation unavailable")?;
    let rows = statement
        .query_map(
            params![
                destination,
                scope.instance,
                scope.bot,
                scope.chat,
                conversation
            ],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?)),
        )
        .map_err(|_| "Queue observation unavailable")?;
    let mut counts = Vec::new();
    for row in rows {
        let (state, count) = row.map_err(|_| "Queue observation unavailable")?;
        counts.push(format!("{state}={count}"));
    }
    Ok(if counts.is_empty() {
        "no outstanding staged deliveries".into()
    } else {
        counts.join(", ")
    })
}

/// Native adapters share the same terminal/session/goal implementations as local controls.
pub struct NativeOperations {
    pub instance: String,
    pub directory: std::path::PathBuf,
    pub config: super::config::ControlConfig,
    pub terminals: crate::adapters::terminals::Terminals,
    pub environment: std::collections::BTreeMap<String, String>,
}
impl NativeOperations {
    fn snapshot(&self) -> Result<serde_json::Value> {
        let mut roots = vec![std::path::PathBuf::from("/tmp")];
        if let Some(path) = self.environment.get("TMUX_TMPDIR") {
            roots.push(path.into());
        }
        let snapshot = super::inventory::collect(&self.config, &self.terminals, &roots);
        if snapshot["errors"]
            .as_array()
            .is_some_and(|errors| !errors.is_empty())
        {
            return Err("Terminal discovery is incomplete; refresh diagnostics".into());
        }
        Ok(snapshot)
    }
    fn pane(&self, target: &Target) -> Result<serde_json::Value> {
        let snapshot = self.snapshot()?;
        let matches: Vec<_> = snapshot["panes"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|pane| {
                crate::adapters::terminals::identity(pane).ok().as_ref() == Some(&target.terminal)
            })
            .collect();
        let pane = matches.first().ok_or("Exact terminal is unavailable")?;
        // Linked tmux views can repeat one physical terminal, but must agree about the conversation.
        if matches
            .iter()
            .any(|other| other.get("agent_session") != pane.get("agent_session"))
        {
            return Err("Ambiguous live terminal identity".into());
        }
        let conversation = pane["agent_session"]["value"]
            .as_str()
            .filter(|_| pane["agent_session"]["kind"] == "id")
            .map(str::to_owned);
        if conversation != target.conversation {
            return Err("Native conversation changed; refresh".into());
        }
        self.terminals.verify(pane)
    }
    fn registration(&self, target: &Target, require_goal: bool) -> Result<std::path::PathBuf> {
        let directory = self.directory.join("hooks");
        let mut found = Vec::new();
        for (index, entry) in std::fs::read_dir(directory)
            .map_err(|_| "No hook registrations for this instance")?
            .enumerate()
        {
            if index >= 1024 {
                return Err("Hook registration limit exceeded".into());
            }
            let path = entry
                .map_err(|_| "Cannot inspect hook registrations")?
                .path();
            if path.extension().is_none_or(|extension| extension != "json") {
                continue;
            }
            let registration = super::hook_runtime::inspect(&path)
                .map_err(|_| "Hook registration is invalid; inspect local diagnostics")?;
            if registration.scope.instance == self.instance
                && registration.scope.terminal == target.terminal
                && Some(&registration.scope.conversation) == target.conversation.as_ref()
                && registration.directory == self.directory
                && (!require_goal || registration.goal_id.is_some())
            {
                found.push(path);
            }
        }
        if found.len() != 1 {
            return Err("Observation requires exactly one registered conversation binding".into());
        }
        Ok(found.remove(0))
    }
}
impl Operations for NativeOperations {
    fn prepare_agent_rename(
        &mut self,
        caller: &Caller,
        target: &Target,
        name: &str,
        base: &str,
    ) -> Result<super::agent_identity::Plan> {
        let pane = self.pane(target)?;
        let event = format!(
            "telegram:{}:{}:{}:{}",
            caller.scope.bot, caller.scope.chat, caller.actor, caller.event_id
        );
        super::agent_identity::Plan::prepare(
            &self.directory,
            super::agent_identity::Request {
                scope: caller.scope.clone(),
                actor: &caller.actor,
                target: name,
                base,
                event: &event,
                pane,
            },
            &mut super::agent_identity::Native {
                terminals: &self.terminals,
                herdr_sockets: self.identity_sockets()?,
            },
        )
    }
    fn agent_rename_status(&mut self, caller: &Caller, operation: &str) -> Result<String> {
        if caller.scope.instance != self.instance {
            return Err("Agent rename instance mismatch".into());
        }
        let receipt = super::agent_identity::status(
            &self.directory,
            &caller.scope,
            &caller.actor,
            operation,
            &mut super::agent_identity::Native {
                terminals: &self.terminals,
                herdr_sockets: self.identity_sockets()?,
            },
        )?;
        Ok(format!(
            "Agent identity rename {}: {}. Operation: {}. Provider conversation unchanged.",
            receipt.state, receipt.display, receipt.operation
        ))
    }

    fn prepare_stream(
        &mut self,
        caller: &Caller,
        target: &Target,
        revision: i64,
        mode: super::telegram_stream::Mode,
    ) -> Result<super::telegram_stream::Plan> {
        self.pane(target)?;
        let config = super::telegram_config::ServiceConfig::load(
            &self.directory.join("telegram-service.json"),
        )?;
        let bindings = config
            .hook_bindings
            .iter()
            .filter(|b| {
                b.route_scope == caller.scope
                    && b.actor == caller.actor
                    && b.route_revision == revision
                    && b.goal_scope.terminal == target.terminal
                    && Some(&b.goal_scope.conversation) == target.conversation.as_ref()
            })
            .collect::<Vec<_>>();
        if bindings.len() != 1 {
            return Err(
                "Streaming needs exactly one reviewed hook recipient binding for this route".into(),
            );
        }
        let registration = config
            .check_registrations()?
            .into_iter()
            .find(|(_, r)| r.id == bindings[0].registration_id && r.scope == bindings[0].goal_scope)
            .ok_or("Stream registration unavailable")?
            .1;
        super::hook_runtime::verify_live_session(&registration, &self.terminals)
            .map_err(|_| "Stream harness is not verified")?;
        if !registration
            .events
            .contains(&super::hooks::EventKind::UserPromptSubmit)
        {
            return Err("Streaming requires verified prompt hooks".into());
        }
        if registration.scope.harness == super::hooks::Harness::Grok {
            return Err("Grok streaming requires verified feeder turn ownership; feeder readiness is not available yet".into());
        }
        if !registration.events.iter().any(|event| {
            matches!(
                event,
                super::hooks::EventKind::Stop | super::hooks::EventKind::AssistantResponse
            )
        }) {
            return Err(
                "Streaming requires enabled assistant response or completed-turn hooks".into(),
            );
        }
        if mode == super::telegram_stream::Mode::Work
            && !registration
                .events
                .contains(&super::hooks::EventKind::PostToolUse)
        {
            return Err("Tool-name streaming requires enabled verified PostToolUse hooks".into());
        }
        Ok(super::telegram_stream::Plan {
            binding: bindings[0].clone(),
            mode,
        })
    }
    fn stream_control(
        &mut self,
        caller: &Caller,
        setting: super::telegram_stream::Setting,
    ) -> Result<String> {
        if caller.scope.instance != self.instance {
            return Err("Stream instance mismatch".into());
        }
        super::telegram_stream::Store::open(&self.directory)?.control(caller, setting)
    }

    fn prepare_session_rename(
        &mut self,
        caller: &Caller,
        target: &Target,
        name: &str,
    ) -> Result<super::telegram_rename::Plan> {
        if caller.scope.instance != self.instance {
            return Err("Rename instance mismatch".into());
        }
        let pane = self.pane(target)?;
        super::telegram_rename::prepare(
            caller,
            &pane,
            name,
            &mut super::telegram_rename::Native(&self.terminals),
        )
    }
    fn session_rename_status(&mut self, caller: &Caller, operation: &str) -> Result<String> {
        if caller.scope.instance != self.instance {
            return Err("Rename instance mismatch".into());
        }
        super::telegram_rename::Store::open(&self.directory)?.status(
            caller,
            operation,
            &mut super::telegram_rename::Native(&self.terminals),
        )
    }

    fn prepare_new(
        &mut self,
        selection: &super::telegram_launch::Selection,
        target: &Target,
        now: u64,
    ) -> Result<super::telegram_launch::Reviewed> {
        let pane = self.pane(target)?;
        super::telegram_launch::prepare(
            selection,
            &pane,
            &self.config,
            &self.directory,
            &self.terminals,
            &self.environment,
            now,
        )
    }
    fn session_scopes(&mut self, caller: &Caller) -> Result<Vec<Scope>> {
        if caller.scope.instance != self.instance {
            return Err("Instance mismatch".into());
        }
        super::diagnostics::with_database(&self.directory.join("routes.sqlite3"), |con| {
            let mut query=con.prepare("SELECT chat FROM chat_routes WHERE instance=? AND bot=? ORDER BY chat LIMIT 10001").map_err(|_| "Session bindings unavailable")?;
            let rows=query.query_map(params![caller.scope.instance,caller.scope.bot],|row| row.get::<_,String>(0)).map_err(|_| "Session bindings unavailable")?;
            let mut scopes=Vec::new();
            for row in rows { scopes.push(Scope { instance:caller.scope.instance.clone(),bot:caller.scope.bot.clone(),chat:row.map_err(|_| "Session bindings unavailable")? }); }
            Ok(scopes)
        }).map(|scopes|scopes.unwrap_or_default())
    }
    fn pending(&mut self, caller: &Caller, target: &Target) -> Result<String> {
        self.pane(target)?;
        if caller.scope.instance != self.instance {
            return Err("Instance mismatch".into());
        }
        let destination =
            serde_json::to_string(&target.terminal).map_err(|_| "Invalid terminal")?;
        let counts =
            super::diagnostics::with_database(&self.directory.join("delivery.sqlite3"), |con| {
                queue_counts(
                    con,
                    &caller.scope,
                    &destination,
                    target.conversation.as_deref(),
                )
            })?;
        Ok(match counts {
            Some(counts) => format!(
                "This chat’s current route queue: {}\nQueued is not delivered; unknown requires local review. Unstaged inbox messages are not included.",
                counts
            ),
            None => "Queue state: Unknown (no delivery journal).".into(),
        })
    }
    fn inventory(&mut self) -> Result<Vec<Terminal>> {
        let snapshot = self.snapshot()?;
        let mut terminals = std::collections::BTreeMap::new();
        for pane in snapshot["panes"].as_array().into_iter().flatten() {
            let identity = crate::adapters::terminals::identity(pane)?;
            let row = Terminal {
                identity: identity.clone(),
                label: super::tree::title(pane, ""),
                workspace: pane["cwd"].as_str().unwrap_or_default().into(),
                harness: pane["agent"].as_str().unwrap_or_default().into(),
                conversation: pane["agent_session"]["value"]
                    .as_str()
                    .filter(|_| pane["agent_session"]["kind"] == "id")
                    .map(str::to_owned),
                available: true,
            };
            if let Some(old) = terminals.insert(terminal_key(&identity), row.clone())
                && old.conversation != row.conversation
            {
                return Err("Conflicting terminal conversations".into());
            }
        }
        Ok(terminals.into_values().collect())
    }
    fn execute(&mut self, action: &Action, target: &Target) -> Result<String> {
        self.execute_native(action, target)
            .map(|text| bounded_text(&text, 4000))
            .map_err(|_| "Native control could not be confirmed. Inspect local diagnostics and current state before retrying.".into())
    }
}
impl NativeOperations {
    fn identity_sockets(&self) -> Result<std::collections::BTreeMap<String, std::path::PathBuf>> {
        let bytes = crate::core::layouts::read_private(&self.directory.join("control.json"))?
            .ok_or("Instance configuration unavailable")?;
        let config = super::config::ControlConfig::parse(&bytes)?;
        if config.agent_name != self.instance {
            return Err("Agent identity instance changed".into());
        }
        Ok(config
            .discovery
            .herdr_sockets
            .into_iter()
            .map(|(session, path)| (session, path.into()))
            .collect())
    }
    fn execute_native(&mut self, action: &Action, target: &Target) -> Result<String> {
        use super::session_actions::{LaunchRequest, SaveRequest, SessionStore};
        use crate::{
            adapters::{
                harnesses, process,
                terminal_actions::{Action as TerminalAction, Placement},
            },
            core::{checkpoints::Operation, goal_store::Control},
        };
        let pane = self.pane(target)?;
        let store = SessionStore {
            directory: self.directory.clone(),
        };
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|_| "Invalid system clock")?
            .as_millis();
        let now = u64::try_from(now).map_err(|_| "Invalid system clock")?;
        match action {
            Action::AgentRename(plan)=>{
                let identity:TerminalId=serde_json::from_value(plan.review()?["terminal"].clone()).map_err(|_|"Invalid agent rename target")?;
                if plan.scope.instance!=self.instance || identity!=target.terminal{return Err("Agent rename target changed".into());}
                let receipt=super::agent_identity::apply(&self.directory,*plan.clone(),&mut super::agent_identity::Native{terminals:&self.terminals,herdr_sockets:self.identity_sockets()?})?;
                Ok(format!("Agent identity rename {}: {}. Operation: {}. Provider conversation unchanged.",receipt.state,receipt.display,receipt.operation))
            }

            Action::StreamStart(plan)=>{
                let config=super::telegram_config::ServiceConfig::load(&self.directory.join("telegram-service.json"))?;
                if !config.hook_bindings.contains(&plan.binding){return Err("Stream recipient binding changed".into());}
                let head=super::diagnostics::with_database(&self.directory.join("delivery.sqlite3"),|con|con.query_row("SELECT COALESCE(MAX(id),0) FROM inbox WHERE channel='hooks' AND bot=?",[&self.instance],|r|r.get::<_,i64>(0)).map_err(|_|"Cannot inspect stream start boundary".into()))?.ok_or("Hook journal unavailable")?;
                super::telegram_stream::Store::open(&self.directory)?.start(plan,head)
            }

            Action::SessionRename(plan) => {
                if plan.scope.instance!=self.instance || crate::adapters::terminals::identity(&plan.pane)?!=target.terminal {return Err("Rename target mismatch".into());}
                super::telegram_rename::Store::open(&self.directory)?.apply(plan,&mut super::telegram_rename::Native(&self.terminals))
            }

            Action::New(reviewed) => super::telegram_launch::apply(reviewed,&pane,&self.config,&self.terminals,now).map(|receipt|format!("{}\nThe current route is unchanged. Telegram feeder enrollment and recipient binding require separate reviewed setup; readiness is not confirmed.",receipt.message)),
            Action::NativeSlash {command,arguments} => {
                let registration=self.registration(target,false)?;
                super::harness_control::submit(super::harness_control::Request{directory:self.directory.clone(),registration,target:pane.clone(),command:command.clone(),arguments:arguments.clone()},&self.config,&self.terminals,&self.environment)
            }
            Action::Capture => self.terminals.capture(&pane, 60).map(|text| {
                text.chars()
                    .rev()
                    .take(1800)
                    .collect::<Vec<_>>()
                    .into_iter()
                    .rev()
                    .collect()
            }),
            Action::Busy => {
                let observation = (|| {
                    let path = self.registration(target, false)?;
                    let registration =
                        super::hook_runtime::inspect(&path).map_err(|_| "Invalid registration")?;
                    super::hook_runtime::verify_live_session(&registration, &self.terminals)
                        .map_err(|_| "Unverified terminal")?;
                    let ready =
                        super::hook_runtime::ready_snapshot(&path, std::path::Path::new("/proc"))
                            .map_err(|_| "Unverified lifecycle")?;
                    Ok::<_, String>(format!(
                        "Hook readiness: {:?} (observation {}). This is not a delivery receipt.",
                        ready.state, ready.sequence
                    ))
                })();
                Ok(observation.unwrap_or_else(|_| "Hook readiness: Unknown. No unique verified live conversation observation is available.".into()))
            }
            Action::Keys(keys) => {
                validate_keys(keys)?;
                self.terminals
                    .action(&pane, TerminalAction::Keys(keys.clone()))
                    .map(|receipt| receipt.message)
            }
            Action::Close => self
                .terminals
                .action(&pane, TerminalAction::Close)
                .map(|receipt| receipt.message),
            Action::Interrupt => self
                .terminals
                .action(&pane, TerminalAction::Interrupt)
                .map(|receipt| receipt.message),
            Action::GoalStatus
            | Action::GoalArm(_)
            | Action::GoalPause
            | Action::GoalResume
            | Action::GoalComplete => {
                let registration = self.registration(target, true)?;
                match action {
                    Action::GoalStatus => (),
                    Action::GoalArm(directive) => {
                        super::hook_runtime::arm(&registration, directive, now)
                            .map_err(|e| e.to_string())?
                    }
                    action => super::hook_runtime::control(
                        &registration,
                        match action {
                            Action::GoalPause => Control::Pause,
                            Action::GoalResume => Control::Resume,
                            Action::GoalComplete => Control::Complete,
                            _ => unreachable!(),
                        },
                        now,
                    )
                    .map_err(|e| e.to_string())?,
                }
                let status =
                    super::hook_runtime::status(&registration, now).map_err(|e| e.to_string())?;
                Ok(format!(
                    "Goal state: {:?}. Arming or resuming policy does not prove the agent is running.",
                    status.goal.status
                ))
            }
            Action::CheckpointList => {
                let rows = store
                    .list()?
                    .into_iter()
                    .filter(|row| {
                        row.checkpoint.source.as_ref().is_some_and(|s| {
                            s.backend == target.terminal.backend
                                && s.server == target.terminal.endpoint
                                && s.pane_id == target.terminal.pane
                                && s.terminal_id == target.terminal.generation
                        })
                    })
                    .take(20)
                    .map(|row| {
                        format!(
                            "{}\n{}",
                            row.checkpoint.name.chars().take(60).collect::<String>(),
                            row.key
                        )
                    })
                    .collect::<Vec<_>>();
                Ok(if rows.is_empty() {
                    "No saved checkpoints for this terminal generation.".into()
                } else {
                    rows.join("\n")
                })
            }
            Action::CheckpointSave(name) => {
                let row = store.save(
                    &self.terminals,
                    &self.config,
                    &self.environment,
                    &pane,
                    &SaveRequest {
                        name: name.clone(),
                        sid: String::new(),
                        include_output: false,
                    },
                )?;
                Ok(format!("Checkpoint saved: {}", row.key))
            }
            Action::CheckpointRemove(key)
            | Action::CheckpointResume(key)
            | Action::CheckpointFork(key) => {
                let row = store.get(key)?;
                if !row.checkpoint.source.as_ref().is_some_and(|source| {
                    source.backend == target.terminal.backend
                        && source.server == target.terminal.endpoint
                        && source.pane_id == target.terminal.pane
                        && source.terminal_id == target.terminal.generation
                }) {
                    return Err("Checkpoint does not belong to this terminal binding".into());
                }
                if matches!(action, Action::CheckpointRemove(_)) {
                    store.remove(key)?;
                    return Ok(
                        "Checkpoint reference removed; native session history is unchanged".into(),
                    );
                }
                let tool =
                    harnesses::tool(&row.checkpoint.harness).ok_or("Unknown checkpoint harness")?;
                let binary = harnesses::executable(&tool, &self.environment)
                    .ok_or("Harness executable is missing")?;
                let plan = harnesses::launch_plan(
                    &tool,
                    &binary,
                    std::path::Path::new(&row.checkpoint.cwd),
                    &self.environment,
                    &self.config.profiles,
                )?;
                let output = process::run(
                    plan.command().arg("--help"),
                    std::time::Duration::from_secs(10),
                    1024 * 1024,
                )
                .map_err(|_| "Native capability probe failed")?;
                if !output.status.success() {
                    return Err("Native capability probe failed".into());
                }
                let help =
                    String::from_utf8(output.stdout).map_err(|_| "Invalid capability response")?;
                let capabilities = harnesses::checkpoint_capabilities(&tool, &binary, Some(&help))?;
                let request = LaunchRequest {
                    operation: if matches!(action, Action::CheckpointFork(_)) {
                        Operation::NativeFork
                    } else {
                        Operation::Resume
                    },
                    target: &capabilities,
                    parent: &pane,
                    placement: Placement::Split,
                };
                let prepared = store.prepare(key, request, &self.config, &self.environment)?;
                store
                    .apply(&self.terminals, prepared)
                    .map(|receipt| receipt.message)
            }
            Action::Select | Action::Pin(_) => {
                Err("Route actions must use the registry transaction".into())
            }
        }
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::core::identity::Backend;
    struct Mock {
        rows: Vec<Terminal>,
        effects: usize,
        fail: bool,
    }
    impl Operations for Mock {
        fn inventory(&mut self) -> Result<Vec<Terminal>> {
            Ok(self.rows.clone())
        }
        fn execute(&mut self, _: &Action, _: &Target) -> Result<String> {
            self.effects += 1;
            if self.fail {
                Err("fixture uncertain outcome".into())
            } else {
                Ok("fixture completed".into())
            }
        }
    }
    fn fixture() -> (Dispatcher, Caller, Registry, Mock) {
        let caller = Caller {
            scope: Scope {
                instance: "example".into(),
                bot: "123".into(),
                chat: "42".into(),
            },
            actor: "7".into(),
            human: true,
            forwarded: false,
            bot_username: "example_bot".into(),
            event_id: "update:1".into(),
        };
        let terminal = Terminal {
            identity: TerminalId {
                backend: Backend::Tmux,
                endpoint: "/fixture.sock".into(),
                pane: "%1".into(),
                generation: "generation".into(),
            },
            label: "workspace".into(),
            workspace: crate::test_support::path("/workspace").into(),
            harness: "claude".into(),
            conversation: Some("conversation".into()),
            available: true,
        };
        let mut routes = Registry::memory().unwrap();
        routes
            .inventory(
                Backend::Tmux,
                "/fixture.sock",
                std::slice::from_ref(&terminal),
                true,
                true,
            )
            .unwrap();
        routes
            .set_grant(
                &caller.scope,
                &caller.actor,
                &[Permission::Read, Permission::Manage, Permission::Send],
            )
            .unwrap();
        routes
            .select(
                &caller.scope,
                &caller.actor,
                &terminal.identity,
                std::slice::from_ref(&terminal.identity),
                0,
                true,
                false,
            )
            .unwrap();
        (
            Dispatcher {
                callbacks: CallbackStore::memory().unwrap(),
            },
            caller,
            routes,
            Mock {
                rows: vec![terminal],
                effects: 0,
                fail: false,
            },
        )
    }
    #[test]
    fn new_launch_prepares_private_review_then_confirms_once_without_leaking_plan() {
        struct Launcher {
            terminal: Terminal,
            prepared: usize,
            applied: usize,
        }
        impl Operations for Launcher {
            fn inventory(&mut self) -> Result<Vec<Terminal>> {
                Ok(vec![self.terminal.clone()])
            }
            fn prepare_new(
                &mut self,
                selection: &super::super::telegram_launch::Selection,
                target: &Target,
                _: u64,
            ) -> Result<super::super::telegram_launch::Reviewed> {
                assert_eq!(selection.harness, "claude");
                assert_eq!(target.terminal, self.terminal.identity);
                self.prepared += 1;
                Ok(super::super::telegram_launch::fixture(
                    serde_json::json!({"backend":"tmux","socket":target.terminal.endpoint,"pane_id":target.terminal.pane,"terminal_id":target.terminal.generation,"cwd":crate::test_support::path("/tmp/project"),"agent_session":{"kind":"id","value":target.conversation}}),
                ))
            }
            fn execute(&mut self, action: &Action, _: &Target) -> Result<String> {
                assert!(matches!(action, Action::New(_)));
                self.applied += 1;
                Ok("Launch submitted; readiness unconfirmed".into())
            }
        }
        let (mut dispatcher, caller, mut routes, operations) = fixture();
        let mut launcher = Launcher {
            terminal: operations.rows[0].clone(),
            prepared: 0,
            applied: 0,
        };
        let command = parse(
            crate::test_support::path("/new claude split"),
            &caller.bot_username,
        )
        .unwrap()
        .unwrap();
        routes
            .set_grant(&caller.scope, &caller.actor, &[Permission::Read])
            .unwrap();
        assert!(
            dispatcher
                .command(&caller, command.clone(), &mut routes, &mut launcher, 1)
                .is_err()
        );
        assert_eq!(launcher.prepared, 0);
        routes
            .set_grant(
                &caller.scope,
                &caller.actor,
                &[Permission::Read, Permission::Manage],
            )
            .unwrap();
        let reply = dispatcher
            .command(&caller, command, &mut routes, &mut launcher, 1)
            .unwrap();
        assert_eq!(launcher.prepared, 1);
        assert_eq!(launcher.applied, 0);
        assert!(!reply.text.contains("DO_NOT_RENDER_PRIVATE_PLAN"));
        assert!(!reply.text.contains("PRIVATE_TEST_VALUE"));
        assert!(reply.text.contains("No Telegram recipient"));
        let token = &reply.buttons[0][0].callback_data;
        dispatcher
            .callback(&caller, token, &mut routes, &mut launcher, 2)
            .unwrap();
        assert_eq!(launcher.applied, 1);
        assert!(
            dispatcher
                .callback(&caller, token, &mut routes, &mut launcher, 3)
                .is_err()
        );
        assert_eq!(launcher.applied, 1);
    }
    #[test]
    fn agent_identity_rename_is_confirmed_scoped_and_keeps_permanent_identity() {
        struct Names {
            row: Terminal,
            applied: usize,
        }
        impl Operations for Names {
            fn inventory(&mut self) -> Result<Vec<Terminal>> {
                Ok(vec![self.row.clone()])
            }
            fn prepare_agent_rename(
                &mut self,
                caller: &Caller,
                target: &Target,
                name: &str,
                base: &str,
            ) -> Result<super::super::agent_identity::Plan> {
                assert_eq!(name, "agent-1");
                serde_json::from_value(serde_json::json!({"scope":caller.scope,"actor":caller.actor,"operation":format!("ar:{}","a".repeat(64)),"display":format!("{base}-1"),"base":base,"binding":{"id":1,"base":"agent","display":"agent-1","backend":"tmux","server":target.terminal.endpoint,"terminal_id":target.terminal.generation,"pane_id":target.terminal.pane},"pane":{},"before":{"terminal":target.terminal,"label":"agent-1","group":"@1","group_label":"agent-1","members":[],"tokens":{}},"event":"fixture"})).map_err(|_|"Invalid fixture".into())
            }
            fn execute(&mut self, action: &Action, _: &Target) -> Result<String> {
                assert!(matches!(action, Action::AgentRename(_)));
                self.applied += 1;
                Ok("verified identity fixture".into())
            }
        }
        let (mut dispatcher, caller, mut routes, operations) = fixture();
        let mut names = Names {
            row: operations.rows[0].clone(),
            applied: 0,
        };
        let command = parse("/rename agent-1 renamed", "example_bot")
            .unwrap()
            .unwrap();
        routes
            .set_grant(&caller.scope, &caller.actor, &[Permission::Read])
            .unwrap();
        assert!(
            dispatcher
                .command(&caller, command.clone(), &mut routes, &mut names, 1)
                .is_err()
        );
        routes
            .set_grant(
                &caller.scope,
                &caller.actor,
                &[Permission::Read, Permission::Manage],
            )
            .unwrap();
        let reply = dispatcher
            .command(&caller, command, &mut routes, &mut names, 1)
            .unwrap();
        assert!(reply.text.contains("renamed-1"));
        assert!(reply.text.contains("permanent ID"));
        assert_eq!(names.applied, 0);
        dispatcher
            .callback(
                &caller,
                &reply.buttons[0][0].callback_data,
                &mut routes,
                &mut names,
                2,
            )
            .unwrap();
        assert_eq!(names.applied, 1);
        assert!(
            dispatcher
                .callback(
                    &caller,
                    &reply.buttons[0][0].callback_data,
                    &mut routes,
                    &mut names,
                    3
                )
                .is_err()
        );
        assert!(parse("/rename guessed", "example_bot").is_err());
        assert!(parse("/rename_status ../state", "example_bot").is_err());
    }
    #[test]
    fn stream_aliases_need_private_chat_all_grants_and_confirmed_start_but_stop_is_immediate() {
        struct Streams {
            row: Terminal,
            starts: usize,
            stops: usize,
        }
        impl Operations for Streams {
            fn inventory(&mut self) -> Result<Vec<Terminal>> {
                Ok(vec![self.row.clone()])
            }
            fn prepare_stream(
                &mut self,
                caller: &Caller,
                target: &Target,
                revision: i64,
                mode: super::super::telegram_stream::Mode,
            ) -> Result<super::super::telegram_stream::Plan> {
                Ok(super::super::telegram_stream::Plan {
                    mode,
                    binding: super::super::hook_effects::Binding {
                        registration_id: "hook".into(),
                        goal_scope: crate::core::goal_store::Scope {
                            instance: caller.scope.instance.clone(),
                            terminal: target.terminal.clone(),
                            conversation: target.conversation.clone().unwrap(),
                            harness: super::super::hooks::Harness::Claude,
                        },
                        route_scope: caller.scope.clone(),
                        actor: caller.actor.clone(),
                        route_revision: revision,
                        reply_to: None,
                        hindsight_bank: None,
                    },
                })
            }
            fn execute(&mut self, action: &Action, _: &Target) -> Result<String> {
                assert!(matches!(action, Action::StreamStart(_)));
                self.starts += 1;
                Ok("enabled fixture".into())
            }
            fn stream_control(
                &mut self,
                _: &Caller,
                setting: super::super::telegram_stream::Setting,
            ) -> Result<String> {
                assert_eq!(setting, super::super::telegram_stream::Setting::Off);
                self.stops += 1;
                Ok("off fixture".into())
            }
        }
        let (mut dispatcher, mut caller, mut routes, operations) = fixture();
        let mut stream = Streams {
            row: operations.rows[0].clone(),
            starts: 0,
            stops: 0,
        };
        let command = parse("/reason on", "example_bot").unwrap().unwrap();
        assert!(
            dispatcher
                .command(&caller, command.clone(), &mut routes, &mut stream, 1)
                .is_err()
        ); // actor7 is not DM42
        caller.scope.chat = caller.actor.clone();
        routes
            .set_grant(
                &caller.scope,
                &caller.actor,
                &[Permission::Read, Permission::Manage, Permission::Send],
            )
            .unwrap();
        routes
            .select(
                &caller.scope,
                &caller.actor,
                &stream.row.identity,
                std::slice::from_ref(&stream.row.identity),
                0,
                true,
                false,
            )
            .unwrap();
        let reply = dispatcher
            .command(&caller, command, &mut routes, &mut stream, 1)
            .unwrap();
        assert_eq!(stream.starts, 0);
        assert!(reply.text.contains("not token-by-token"));
        dispatcher
            .callback(
                &caller,
                &reply.buttons[0][0].callback_data,
                &mut routes,
                &mut stream,
                2,
            )
            .unwrap();
        assert_eq!(stream.starts, 1);
        assert!(
            dispatcher
                .callback(
                    &caller,
                    &reply.buttons[0][0].callback_data,
                    &mut routes,
                    &mut stream,
                    3
                )
                .is_err()
        );
        let stop = parse("/verbose off", "example_bot").unwrap().unwrap();
        assert!(
            dispatcher
                .command(&caller, stop, &mut routes, &mut stream, 4)
                .unwrap()
                .buttons
                .is_empty()
        );
        assert_eq!(stream.stops, 1);
        assert_eq!(
            parse("/verbose on", "example_bot").unwrap(),
            parse("/reason on", "example_bot").unwrap()
        );
        assert!(parse("/stream raw", "example_bot").is_err());
    }
    #[test]
    fn removed_raw_console_export_is_not_pending_or_sent_to_agent() {
        let (mut dispatcher, caller, mut routes, mut operations) = fixture();
        for input in ["/mirror on PRIVATE_ARGUMENT", "/console@example_bot on"] {
            let command = parse(input, &caller.bot_username).unwrap().unwrap();
            assert_eq!(command, Command::Unavailable("mirror".into()));
            let reply = dispatcher
                .command(&caller, command, &mut routes, &mut operations, 1)
                .unwrap();
            assert!(reply.text.contains("was removed"));
            assert!(!reply.text.contains("PRIVATE_ARGUMENT"));
            assert!(reply.buttons.is_empty());
        }
        assert_eq!(operations.effects, 0);
    }
    #[test]
    fn unsupported_permission_mode_is_not_queued_or_sent_to_harness() {
        let (mut dispatcher, caller, mut routes, mut operations) = fixture();
        let command = parse("/mode acceptEdits", &caller.bot_username)
            .unwrap()
            .unwrap();
        assert_eq!(command, Command::Unavailable("mode".into()));
        let reply = dispatcher
            .command(&caller, command, &mut routes, &mut operations, 1)
            .unwrap();
        assert!(reply.text.contains("unsupported"));
        assert_eq!(operations.effects, 0);
    }
    #[test]
    fn deployment_license_admin_is_removed_not_queued_or_sent_to_harness() {
        let (mut dispatcher, caller, mut routes, mut operations) = fixture();
        for input in [
            "/license",
            "/lic provider-token-value",
            "/license@example_bot legacy-account",
        ] {
            let command = parse(input, &caller.bot_username).unwrap().unwrap();
            assert_eq!(command, Command::Unavailable("license".into()));
            let reply = dispatcher
                .command(&caller, command, &mut routes, &mut operations, 1)
                .unwrap();
            assert!(reply.text.contains("was removed"));
            assert!(!reply.text.contains("provider-token-value"));
            assert!(reply.buttons.is_empty());
        }
        assert_eq!(operations.effects, 0);
    }
    #[test]
    fn session_rename_requires_manage_and_confirmation_status_requires_read() {
        struct Naming {
            row: Terminal,
            prepared: usize,
            effects: usize,
        }
        impl Operations for Naming {
            fn inventory(&mut self) -> Result<Vec<Terminal>> {
                Ok(vec![self.row.clone()])
            }
            fn prepare_session_rename(
                &mut self,
                caller: &Caller,
                target: &Target,
                name: &str,
            ) -> Result<super::super::telegram_rename::Plan> {
                self.prepared += 1;
                Ok(super::super::telegram_rename::Plan {
                    scope: caller.scope.clone(),
                    actor: caller.actor.clone(),
                    pane: serde_json::json!({}),
                    name: name.into(),
                    operation: format!("sr:{}", "a".repeat(64)),
                    before: super::super::telegram_rename::Observation {
                        endpoint: target.terminal.endpoint.clone(),
                        session: "$1".into(),
                        server_generation: "10:20".into(),
                        label: "project".into(),
                        members: Default::default(),
                        clients: Default::default(),
                    },
                })
            }
            fn execute(&mut self, action: &Action, _: &Target) -> Result<String> {
                assert!(matches!(action, Action::SessionRename(_)));
                self.effects += 1;
                Ok("rename verified fixture".into())
            }
            fn session_rename_status(&mut self, caller: &Caller, _: &str) -> Result<String> {
                assert_eq!(caller.scope.chat, "42");
                Ok("status fixture".into())
            }
        }
        let (mut dispatcher, caller, mut routes, operations) = fixture();
        let mut naming = Naming {
            row: operations.rows[0].clone(),
            prepared: 0,
            effects: 0,
        };
        let command = parse("/session_rename project-new", "example_bot")
            .unwrap()
            .unwrap();
        routes
            .set_grant(&caller.scope, &caller.actor, &[Permission::Read])
            .unwrap();
        assert!(
            dispatcher
                .command(&caller, command.clone(), &mut routes, &mut naming, 0)
                .is_err()
        );
        assert_eq!(naming.prepared, 0);
        let status = parse(
            &format!("/session_rename_status sr:{}", "a".repeat(64)),
            "example_bot",
        )
        .unwrap()
        .unwrap();
        assert!(
            dispatcher
                .command(&caller, status, &mut routes, &mut naming, 0)
                .unwrap()
                .text
                .contains("status fixture")
        );
        routes
            .set_grant(
                &caller.scope,
                &caller.actor,
                &[Permission::Read, Permission::Manage],
            )
            .unwrap();
        let reply = dispatcher
            .command(&caller, command, &mut routes, &mut naming, 1)
            .unwrap();
        assert_eq!(naming.effects, 0);
        dispatcher
            .callback(
                &caller,
                &reply.buttons[0][0].callback_data,
                &mut routes,
                &mut naming,
                2,
            )
            .unwrap();
        assert_eq!(naming.effects, 1);
        assert!(
            dispatcher
                .callback(
                    &caller,
                    &reply.buttons[0][0].callback_data,
                    &mut routes,
                    &mut naming,
                    3
                )
                .is_err()
        );
        assert!(parse("/session_rename old new", "example_bot").is_err());
        assert_eq!(
            parse("/session_rename@other_bot new", "example_bot").unwrap(),
            None
        );
        assert!(matches!(
            parse("/rename old new", "example_bot").unwrap(),
            Some(Command::AgentRename { .. })
        ));
    }
    #[test]
    fn close_requires_manage_confirmation_and_original_generation() {
        let (mut dispatcher, caller, mut routes, mut operations) = fixture();
        assert_eq!(parse("/kill", "example_bot").unwrap(), Some(Command::Close));
        assert!(parse("/kill guessed-label", "example_bot").is_err());
        routes
            .set_grant(&caller.scope, &caller.actor, &[Permission::Read])
            .unwrap();
        assert!(
            dispatcher
                .command(&caller, Command::Close, &mut routes, &mut operations, 0)
                .is_err()
        );
        routes
            .set_grant(
                &caller.scope,
                &caller.actor,
                &[Permission::Read, Permission::Manage],
            )
            .unwrap();
        let reply = dispatcher
            .command(&caller, Command::Close, &mut routes, &mut operations, 1)
            .unwrap();
        assert_eq!(operations.effects, 0);
        assert!(reply.text.contains("close"));
        let token = &reply.buttons[0][0].callback_data;
        operations.rows[0].identity.generation = "replacement".into();
        assert!(
            dispatcher
                .callback(&caller, token, &mut routes, &mut operations, 2)
                .is_err()
        );
        operations.rows[0].identity.generation = "generation".into();
        dispatcher
            .callback(&caller, token, &mut routes, &mut operations, 3)
            .unwrap();
        assert_eq!(operations.effects, 1);
        assert!(
            dispatcher
                .callback(&caller, token, &mut routes, &mut operations, 4)
                .is_err()
        );
    }
    #[test]
    fn say_is_explicit_text_and_cannot_use_direct_operations() {
        let (mut dispatcher, caller, mut routes, mut operations) = fixture();
        let command = parse("/say@example_bot /review this", &caller.bot_username)
            .unwrap()
            .unwrap();
        assert_eq!(command, Command::Say("/review this".into()));
        assert!(
            dispatcher
                .command(&caller, command, &mut routes, &mut operations, 0)
                .is_err()
        );
        assert_eq!(operations.effects, 0);
        assert!(parse("/say ", "example_bot").is_err());
        assert_eq!(parse("/say@other_bot text", "example_bot").unwrap(), None);
    }
    #[test]
    fn terminal_keys_are_confirmed_once_and_read_grants_cannot_inject() {
        for text in ["/stop", "/esc", "/enter", "/keys C-c Enter"] {
            let (mut dispatcher, caller, mut routes, mut operations) = fixture();
            let command = parse(text, &caller.bot_username).unwrap().unwrap();
            assert!(matches!(command, Command::Keys(_)));
            routes
                .set_grant(&caller.scope, &caller.actor, &[Permission::Read])
                .unwrap();
            assert!(
                dispatcher
                    .command(&caller, command.clone(), &mut routes, &mut operations, 1)
                    .is_err()
            );
            routes
                .set_grant(
                    &caller.scope,
                    &caller.actor,
                    &[Permission::Read, Permission::Manage],
                )
                .unwrap();
            let reply = dispatcher
                .command(&caller, command, &mut routes, &mut operations, 1)
                .unwrap();
            assert_eq!(operations.effects, 0);
            let token = &reply.buttons[0][0].callback_data;
            dispatcher
                .callback(&caller, token, &mut routes, &mut operations, 2)
                .unwrap();
            assert_eq!(operations.effects, 1);
            assert!(
                dispatcher
                    .callback(&caller, token, &mut routes, &mut operations, 3)
                    .is_err()
            );
        }
        for text in [
            "/keys",
            "/keys -l",
            "/keys $(touch /tmp/no)",
            "/enter extra",
            "/stop extra",
        ] {
            assert!(parse(text, "example_bot").is_err());
        }
        assert!(parse(&format!("/keys {}", "Enter ".repeat(65)), "example_bot").is_err());
        assert_eq!(parse("/esc@other_bot", "example_bot").unwrap(), None);
    }
    #[test]
    fn chat_policy_commands_need_manage_but_not_a_live_terminal() {
        let (mut dispatcher, caller, _, mut operations) = fixture();
        let mut routes = Registry::memory().unwrap();
        operations.rows.clear();
        routes
            .set_grant(&caller.scope, &caller.actor, &[Permission::Read])
            .unwrap();
        assert!(
            dispatcher
                .command(
                    &caller,
                    Command::Listen(true),
                    &mut routes,
                    &mut operations,
                    1
                )
                .is_err()
        );
        routes
            .set_grant(
                &caller.scope,
                &caller.actor,
                &[Permission::Read, Permission::Manage],
            )
            .unwrap();
        for (text, listen) in [("/listen", true), ("/quiet", false)] {
            let command = parse(text, &caller.bot_username).unwrap().unwrap();
            let result = dispatcher
                .command(&caller, command, &mut routes, &mut operations, 1)
                .unwrap();
            assert!(result.buttons.is_empty());
            assert_eq!(
                routes.input_policy(&caller.scope).unwrap().listen,
                Some(listen)
            );
        }
        assert_eq!(operations.effects, 0);
        let mut forwarded = caller.clone();
        forwarded.forwarded = true;
        assert!(
            dispatcher
                .command(
                    &forwarded,
                    Command::Listen(true),
                    &mut routes,
                    &mut operations,
                    2
                )
                .is_err()
        );
        assert!(parse("/listen extra", &caller.bot_username).is_err());
        assert_eq!(
            parse("/quiet@other_bot", &caller.bot_username).unwrap(),
            None
        );
        let key = format!("checkpoint:{}", "a".repeat(64));
        assert_eq!(
            parse(&format!("/resume {key}"), &caller.bot_username).unwrap(),
            Some(Command::Checkpoint(Action::CheckpointResume(key)))
        );
        assert!(parse("/resume ambiguous-name", &caller.bot_username).is_err());
    }
    #[test]
    fn native_slash_commands_require_confirmation_and_explain_effects() {
        let (mut dispatcher, caller, mut routes, mut operations) = fixture();
        let command = parse("/compact preserve decisions", &caller.bot_username)
            .unwrap()
            .unwrap();
        routes
            .set_grant(&caller.scope, &caller.actor, &[Permission::Read])
            .unwrap();
        assert!(
            dispatcher
                .command(&caller, command.clone(), &mut routes, &mut operations, 1)
                .is_err()
        );
        routes
            .set_grant(
                &caller.scope,
                &caller.actor,
                &[Permission::Read, Permission::Manage],
            )
            .unwrap();
        let reply = dispatcher
            .command(&caller, command, &mut routes, &mut operations, 1)
            .unwrap();
        assert_eq!(operations.effects, 0);
        assert!(reply.text.contains("May invoke model work: true"));
        assert!(reply.text.contains("Changes context: true"));
        let token = &reply.buttons[0][0].callback_data;
        dispatcher
            .callback(&caller, token, &mut routes, &mut operations, 2)
            .unwrap();
        assert_eq!(operations.effects, 1);
        assert!(
            dispatcher
                .callback(&caller, token, &mut routes, &mut operations, 3)
                .is_err()
        );
        for text in [
            "/cc /unknown",
            "/compact $(command)",
            crate::test_support::path("/config bypass"),
            "/effort extreme",
        ] {
            assert!(parse(text, &caller.bot_username).is_err());
        }
        assert_eq!(
            parse("/model@other_bot sonnet", &caller.bot_username).unwrap(),
            None
        );
    }
    #[test]
    fn observations_are_connected_read_controls_and_scope_is_passed() {
        struct Observe {
            terminal: Terminal,
        }
        impl Operations for Observe {
            fn inventory(&mut self) -> Result<Vec<Terminal>> {
                Ok(vec![self.terminal.clone()])
            }
            fn session_scopes(&mut self, caller: &Caller) -> Result<Vec<Scope>> {
                let mut denied = caller.scope.clone();
                denied.chat = "secret-chat".into();
                Ok(vec![caller.scope.clone(), denied])
            }
            fn execute(&mut self, action: &Action, _: &Target) -> Result<String> {
                assert!(matches!(action, Action::Busy | Action::CheckpointList));
                Ok("verified fixture observation".into())
            }
            fn pending(&mut self, caller: &Caller, target: &Target) -> Result<String> {
                assert_eq!(caller.scope.chat, "42");
                assert_eq!(target.terminal, self.terminal.identity);
                Ok("scoped fixture queue".into())
            }
        }
        let (mut dispatcher, caller, mut routes, operations) = fixture();
        routes
            .set_grant(&caller.scope, &caller.actor, &[Permission::Read])
            .unwrap();
        let mut operations = Observe {
            terminal: operations.rows[0].clone(),
        };
        for text in ["/sessions", "/busy", "/pending"] {
            let command = parse(text, &caller.bot_username).unwrap().unwrap();
            let reply = dispatcher
                .command(&caller, command, &mut routes, &mut operations, 1)
                .unwrap();
            assert!(reply.buttons.is_empty());
            if text == "/sessions" {
                assert!(reply.text.contains("Chat 42"));
                assert!(!reply.text.contains("secret-chat"));
            } else {
                assert!(reply.text.contains("fixture"));
            }
        }
    }
    #[test]
    fn queue_counts_never_include_other_chat_generation_or_conversation() {
        let con = Connection::open_in_memory().unwrap();
        con.execute_batch(
            "CREATE TABLE delivery(direction TEXT,destination TEXT,payload TEXT,state TEXT);",
        )
        .unwrap();
        let (_, caller, _, operations) = fixture();
        let destination = serde_json::to_string(&operations.rows[0].identity).unwrap();
        for (chat, instance, conversation, dest, state) in [
            (
                "42",
                "example",
                "conversation",
                destination.as_str(),
                "pending",
            ),
            (
                "43",
                "example",
                "conversation",
                destination.as_str(),
                "pending",
            ),
            (
                "42",
                "other",
                "conversation",
                destination.as_str(),
                "pending",
            ),
            (
                "42",
                "example",
                "old-conversation",
                destination.as_str(),
                "unknown",
            ),
            ("42", "example", "conversation", "old-generation", "pending"),
            (
                "42",
                "example",
                "conversation",
                destination.as_str(),
                "delivered",
            ),
            (
                "42",
                "example",
                "conversation",
                destination.as_str(),
                "unknown",
            ),
        ] {
            let payload = serde_json::json!({"scope":{"chat":chat,"instance":instance,"bot":"123"},"conversation":conversation,"text":"PRIVATE PAYLOAD"});
            con.execute(
                "INSERT INTO delivery VALUES('inbound',?,?,?)",
                params![dest, payload.to_string(), state],
            )
            .unwrap();
        }
        let counts = queue_counts(&con, &caller.scope, &destination, Some("conversation")).unwrap();
        assert_eq!(counts, "pending=1, unknown=1");
        assert!(!counts.contains("PRIVATE"));
        assert_eq!(
            queue_counts(&con, &caller.scope, &destination, None).unwrap(),
            "no outstanding staged deliveries"
        );
    }
    #[test]
    fn parser_scopes_suffix_and_never_interprets_labels_or_shell() {
        assert_eq!(parse("/status@OtherBot", "example_bot").unwrap(), None);
        assert!(parse("/invented", "example_bot").is_err());
        assert_eq!(
            parse("/mirror", "example_bot").unwrap(),
            Some(Command::Unavailable("mirror".into()))
        );
        assert_eq!(
            parse("/status@example_bot", "example_bot").unwrap(),
            Some(Command::Status)
        );
        assert!(parse("/use workspace", "example_bot").is_err());
        assert!(parse("/checkpoint remove ../../private", "example_bot").is_err());
        assert!(matches!(
            parse("/goal arm finish tests", "example_bot").unwrap(),
            Some(Command::Goal(Action::GoalArm(_)))
        ));
    }
    #[test]
    fn control_grants_human_scope_and_no_effects_before_confirmation() {
        let (mut dispatcher, mut caller, mut routes, mut operations) = fixture();
        caller.human = false;
        assert!(
            dispatcher
                .command(&caller, Command::Interrupt, &mut routes, &mut operations, 0)
                .is_err()
        );
        caller.human = true;
        caller.forwarded = true;
        assert!(
            dispatcher
                .command(&caller, Command::Interrupt, &mut routes, &mut operations, 0)
                .is_err()
        );
        caller.forwarded = false;
        routes
            .set_grant(&caller.scope, &caller.actor, &[Permission::Read])
            .unwrap();
        assert!(
            dispatcher
                .command(&caller, Command::Interrupt, &mut routes, &mut operations, 0)
                .is_err()
        );
        assert!(
            dispatcher
                .command(&caller, Command::Status, &mut routes, &mut operations, 0)
                .is_ok()
        );
        assert_eq!(operations.effects, 0);
    }
    #[test]
    fn confirmation_is_actor_chat_revision_and_generation_bound_and_one_use() {
        let (mut dispatcher, caller, mut routes, mut operations) = fixture();
        let reply = dispatcher
            .command(&caller, Command::Interrupt, &mut routes, &mut operations, 1)
            .unwrap();
        let token = &reply.buttons[0][0].callback_data;
        assert_eq!(operations.effects, 0);
        assert!(token.len() <= 64);
        let mut wrong = caller.clone();
        wrong.actor = "8".into();
        routes
            .set_grant(
                &wrong.scope,
                &wrong.actor,
                &[Permission::Read, Permission::Manage],
            )
            .unwrap();
        assert!(
            dispatcher
                .callback(&wrong, token, &mut routes, &mut operations, 2)
                .is_err()
        );
        let mut wrong = caller.clone();
        wrong.scope.chat = "99".into();
        routes
            .set_grant(
                &wrong.scope,
                &wrong.actor,
                &[Permission::Read, Permission::Manage],
            )
            .unwrap();
        assert!(
            dispatcher
                .callback(&wrong, token, &mut routes, &mut operations, 2)
                .is_err()
        );
        operations.rows[0].conversation = Some("new conversation".into());
        assert!(
            dispatcher
                .callback(&caller, token, &mut routes, &mut operations, 2)
                .is_err()
        );
        operations.rows[0].conversation = Some("conversation".into());
        dispatcher
            .callback(&caller, token, &mut routes, &mut operations, 2)
            .unwrap();
        assert_eq!(operations.effects, 1);
        assert!(
            dispatcher
                .callback(&caller, token, &mut routes, &mut operations, 3)
                .is_err()
        );
        assert_eq!(operations.effects, 1);
    }
    #[test]
    fn route_change_expiry_and_failure_cannot_replay() {
        let (mut dispatcher, caller, mut routes, mut operations) = fixture();
        let reply = dispatcher
            .command(&caller, Command::Interrupt, &mut routes, &mut operations, 1)
            .unwrap();
        let token = &reply.buttons[0][0].callback_data;
        assert!(
            dispatcher
                .callback(&caller, token, &mut routes, &mut operations, 300001)
                .is_err()
        );
        routes
            .set_pinned(&caller.scope, &caller.actor, 1, true)
            .unwrap();
        assert!(
            dispatcher
                .callback(&caller, token, &mut routes, &mut operations, 2)
                .is_err()
        );
        assert_eq!(operations.effects, 0);
        let reply = dispatcher
            .command(&caller, Command::Interrupt, &mut routes, &mut operations, 3)
            .unwrap();
        let token = &reply.buttons[0][0].callback_data;
        operations.fail = true;
        assert!(
            dispatcher
                .callback(&caller, token, &mut routes, &mut operations, 4)
                .is_err()
        );
        operations.fail = false;
        assert!(
            dispatcher
                .callback(&caller, token, &mut routes, &mut operations, 5)
                .is_err()
        );
        assert_eq!(operations.effects, 1);
    }
    #[test]
    fn select_and_pin_change_actual_registry_only_after_click() {
        let (mut dispatcher, caller, mut routes, mut operations) = fixture();
        let reply = dispatcher
            .command(&caller, Command::Pin(true), &mut routes, &mut operations, 1)
            .unwrap();
        assert!(
            !routes
                .inspect(&caller.scope, &caller.actor)
                .unwrap()
                .unwrap()
                .pinned
        );
        dispatcher
            .callback(
                &caller,
                &reply.buttons[0][0].callback_data,
                &mut routes,
                &mut operations,
                2,
            )
            .unwrap();
        assert!(
            routes
                .inspect(&caller.scope, &caller.actor)
                .unwrap()
                .unwrap()
                .pinned
        );
        assert_eq!(operations.effects, 0);
        let reply = dispatcher
            .command(
                &caller,
                Command::Use(Some(terminal_key(&operations.rows[0].identity))),
                &mut routes,
                &mut operations,
                3,
            )
            .unwrap();
        dispatcher
            .callback(
                &caller,
                &reply.buttons[0][0].callback_data,
                &mut routes,
                &mut operations,
                4,
            )
            .unwrap();
        assert_eq!(
            routes
                .inspect(&caller.scope, &caller.actor)
                .unwrap()
                .unwrap()
                .revision,
            3
        );
    }
    #[test]
    fn confirmation_consumption_survives_restart_and_grant_revocation() {
        use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
        let (mut dispatcher, caller, mut routes, mut operations) = fixture();
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let directory =
            std::env::temp_dir().join(format!("daedalus-controls-{}-{nonce}", std::process::id()));
        std::fs::create_dir(&directory).unwrap();
        std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o700)).unwrap();
        let path = directory.join("callbacks.sqlite");
        drop(
            std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&path)
                .unwrap(),
        );
        dispatcher.callbacks =
            CallbackStore::from_connection(Connection::open(&path).unwrap()).unwrap();
        let reply = dispatcher
            .command(&caller, Command::Interrupt, &mut routes, &mut operations, 1)
            .unwrap();
        let token = &reply.buttons[0][0].callback_data;
        drop(dispatcher);
        let mut dispatcher = Dispatcher {
            callbacks: CallbackStore::from_connection(Connection::open(&path).unwrap()).unwrap(),
        };
        routes
            .set_grant(&caller.scope, &caller.actor, &[Permission::Read])
            .unwrap();
        assert!(
            dispatcher
                .callback(&caller, token, &mut routes, &mut operations, 2)
                .is_err()
        );
        assert_eq!(operations.effects, 0);
        routes
            .set_grant(
                &caller.scope,
                &caller.actor,
                &[Permission::Read, Permission::Manage],
            )
            .unwrap();
        // Simulate a crash after durable consumption, before recording an outcome.
        let claim = dispatcher.callbacks.peek(&caller, token, 3).unwrap();
        dispatcher
            .callbacks
            .consume(&caller, token, &claim, 3)
            .unwrap();
        drop(dispatcher);
        let mut dispatcher = Dispatcher {
            callbacks: CallbackStore::from_connection(Connection::open(&path).unwrap()).unwrap(),
        };
        assert!(
            dispatcher
                .callback(&caller, token, &mut routes, &mut operations, 4)
                .is_err()
        );
        assert_eq!(operations.effects, 0);
        // Re-admitting the original Telegram event cannot generate a fresh token.
        let replay = dispatcher
            .command(&caller, Command::Interrupt, &mut routes, &mut operations, 5)
            .unwrap();
        assert_eq!(replay.buttons[0][0].callback_data, *token);
        drop(dispatcher);
        std::fs::remove_dir_all(directory).unwrap();
    }
    #[test]
    fn freshly_discovered_terminal_can_be_selected_without_prior_route_cache() {
        let (mut dispatcher, caller, mut routes, mut operations) = fixture();
        operations.rows[0].identity.pane = "%99".into();
        operations.rows[0].identity.generation = "new-generation".into();
        let reply = dispatcher
            .command(
                &caller,
                Command::Use(Some(terminal_key(&operations.rows[0].identity))),
                &mut routes,
                &mut operations,
                1,
            )
            .unwrap();
        dispatcher
            .callback(
                &caller,
                &reply.buttons[0][0].callback_data,
                &mut routes,
                &mut operations,
                2,
            )
            .unwrap();
        assert_eq!(
            routes
                .inspect(&caller.scope, &caller.actor)
                .unwrap()
                .unwrap()
                .terminal,
            operations.rows[0].identity
        );
    }
}
