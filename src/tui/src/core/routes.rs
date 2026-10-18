//! Core terminal registry and chat authorization, independent of Armonia.
//!
//! The service owns the connection and authenticates the caller's bot/user IDs.
//! Display labels never resolve routing or authorization. Revalidate the returned
//! exact terminal at the side-effect boundary: an inventory snapshot is not a lock.
use super::identity::{Backend, TerminalId};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeSet, HashSet};

pub type Result<T> = std::result::Result<T, Error>;
#[derive(Debug, PartialEq, Eq)]
pub enum Error {
    Database,
    Invalid(&'static str),
    Unauthorized,
    Conflict,
    Unavailable,
    Ambiguous,
}
impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Database => f.write_str("Route database operation failed"),
            Self::Invalid(s) => f.write_str(s),
            Self::Unauthorized => f.write_str("Route operation is not authorized"),
            Self::Conflict => f.write_str("Route changed; refresh before selecting"),
            Self::Unavailable => f.write_str("Exact terminal is unavailable"),
            Self::Ambiguous => f.write_str("Terminal identity is ambiguous"),
        }
    }
}
impl std::error::Error for Error {}
impl From<rusqlite::Error> for Error {
    fn from(_: rusqlite::Error) -> Self {
        Self::Database
    }
}
fn encode<T: Serialize>(value: &T) -> Result<String> {
    serde_json::to_string(value).map_err(|_| Error::Invalid("Cannot encode route state"))
}
fn decode<T: serde::de::DeserializeOwned>(value: &str) -> Result<T> {
    serde_json::from_str(value).map_err(|_| Error::Invalid("Invalid saved route state"))
}
fn text(value: &str, empty: bool) -> Result<()> {
    if (!empty && value.is_empty()) || value.len() > 4096 || value.chars().any(char::is_control) {
        Err(Error::Invalid("Invalid route metadata"))
    } else {
        Ok(())
    }
}
fn exact(target: &TerminalId, live: &[TerminalId]) -> Result<()> {
    target.validate().map_err(Error::Invalid)?;
    match live.iter().filter(|id| *id == target).count() {
        0 => Err(Error::Unavailable),
        1 => Ok(()),
        _ => Err(Error::Ambiguous),
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Scope {
    pub instance: String,
    pub bot: String,
    pub chat: String,
}
impl Scope {
    pub fn validate(&self) -> Result<()> {
        for value in [&self.instance, &self.bot, &self.chat] {
            text(value, false)?;
        }
        Ok(())
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Permission {
    Read = 1,
    Send = 2,
    Manage = 4,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Terminal {
    pub identity: TerminalId,
    pub label: String,
    pub workspace: String,
    pub harness: String,
    pub conversation: Option<String>,
    pub available: bool,
}
impl Terminal {
    pub(crate) fn validate(&self) -> Result<()> {
        self.identity.validate().map_err(Error::Invalid)?;
        for value in [&self.label, &self.workspace, &self.harness] {
            text(value, true)?;
        }
        if let Some(sid) = &self.conversation {
            text(sid, false)?;
        }
        Ok(())
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Route {
    pub scope: Scope,
    pub terminal: TerminalId,
    pub pinned: bool,
    pub revision: i64,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InputPolicy {
    /// None inherits the locally configured listen_chats setting.
    pub listen: Option<bool>,
    pub revision: i64,
}
fn input_policy(con: &Connection, scope: &Scope) -> Result<InputPolicy> {
    scope.validate()?;
    let row = con
        .query_row(
            "SELECT listen,revision FROM chat_input_policy WHERE instance=? AND bot=? AND chat=?",
            params![scope.instance, scope.bot, scope.chat],
            |r| {
                Ok(InputPolicy {
                    listen: Some(r.get(0)?),
                    revision: r.get(1)?,
                })
            },
        )
        .optional()?;
    Ok(row.unwrap_or(InputPolicy {
        listen: None,
        revision: 0,
    }))
}
const SCHEMA:&str="
CREATE TABLE IF NOT EXISTS chat_input_policy(instance TEXT NOT NULL,bot TEXT NOT NULL,chat TEXT NOT NULL,listen INTEGER NOT NULL CHECK(listen IN (0,1)),revision INTEGER NOT NULL CHECK(revision>0),PRIMARY KEY(instance,bot,chat));
CREATE TABLE IF NOT EXISTS route_terminals(identity TEXT PRIMARY KEY,backend TEXT NOT NULL,endpoint TEXT NOT NULL,pane TEXT NOT NULL,generation TEXT NOT NULL,label TEXT NOT NULL,workspace TEXT NOT NULL,harness TEXT NOT NULL,conversation TEXT,available INTEGER NOT NULL,UNIQUE(backend,endpoint,pane,generation));
CREATE TABLE IF NOT EXISTS route_grants(instance TEXT NOT NULL,bot TEXT NOT NULL,chat TEXT NOT NULL,actor TEXT NOT NULL,permissions INTEGER NOT NULL,PRIMARY KEY(instance,bot,chat,actor));
CREATE TABLE IF NOT EXISTS chat_routes(instance TEXT NOT NULL,bot TEXT NOT NULL,chat TEXT NOT NULL,identity TEXT NOT NULL REFERENCES route_terminals(identity),pinned INTEGER NOT NULL,revision INTEGER NOT NULL,PRIMARY KEY(instance,bot,chat));
CREATE TABLE IF NOT EXISTS route_imports(instance TEXT NOT NULL,bot TEXT NOT NULL,chat TEXT NOT NULL,plan TEXT NOT NULL,PRIMARY KEY(instance,bot,chat));
CREATE INDEX IF NOT EXISTS route_terminal_endpoint ON route_terminals(backend,endpoint,available);
";
fn backend(value: &Backend) -> &'static str {
    match value {
        Backend::Tmux => "tmux",
        Backend::Herdr => "herdr",
    }
}

pub struct Registry {
    con: Connection,
}
impl Registry {
    /// The caller must open a private owned SQLite file safely. No inherited
    /// default path or Armonia database is consulted by this module.
    pub fn from_connection(mut con: Connection) -> Result<Self> {
        con.busy_timeout(std::time::Duration::from_secs(5))?;
        con.execute_batch("PRAGMA foreign_keys=ON; PRAGMA synchronous=FULL;")?;
        let tx = con.transaction_with_behavior(TransactionBehavior::Immediate)?;
        tx.execute_batch(SCHEMA)?;
        tx.commit()?;
        Ok(Self { con })
    }
    pub fn memory() -> Result<Self> {
        Self::from_connection(Connection::open_in_memory()?)
    }
    /// Local setup/administration only. Telegram payloads must never directly
    /// call this method or supply their own granted permissions.
    pub fn set_grant(
        &mut self,
        scope: &Scope,
        actor: &str,
        permissions: &[Permission],
    ) -> Result<()> {
        scope.validate()?;
        text(actor, false)?;
        let mask = permissions.iter().fold(0_i64, |mask, p| mask | *p as i64);
        self.con.execute("INSERT INTO route_grants(instance,bot,chat,actor,permissions) VALUES(?,?,?,?,?) ON CONFLICT(instance,bot,chat,actor) DO UPDATE SET permissions=excluded.permissions",params![scope.instance,scope.bot,scope.chat,actor,mask])?;
        Ok(())
    }
    /// Replace this service's local authorization policy atomically. Omitted
    /// grants are revoked; saved routes never imply continuing permission.
    pub fn replace_grants(
        &mut self,
        instance: &str,
        bot: &str,
        grants: &[(String, String, Vec<Permission>)],
    ) -> Result<()> {
        text(instance, false)?;
        text(bot, false)?;
        if grants.len() > 10000 {
            return Err(Error::Invalid("Too many route grants"));
        }
        let mut seen = BTreeSet::new();
        for (chat, actor, _) in grants {
            text(chat, false)?;
            text(actor, false)?;
            if !seen.insert((chat, actor)) {
                return Err(Error::Invalid("Duplicate route grant"));
            }
        }
        let tx = self
            .con
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        tx.execute(
            "DELETE FROM route_grants WHERE instance=? AND bot=?",
            params![instance, bot],
        )?;
        for (chat, actor, permissions) in grants {
            let mask = permissions.iter().fold(0i64, |mask, p| mask | *p as i64);
            tx.execute(
                "INSERT INTO route_grants(instance,bot,chat,actor,permissions) VALUES(?,?,?,?,?)",
                params![instance, bot, chat, actor, mask],
            )?;
        }
        tx.commit()?;
        Ok(())
    }
    pub fn input_policy(&self, scope: &Scope) -> Result<InputPolicy> {
        input_policy(&self.con, scope)
    }
    /// Explicit human configuration operation; local grants are rechecked in
    /// the same transaction as the revision update. No terminal route is needed.
    pub fn set_input_policy(
        &mut self,
        scope: &Scope,
        actor: &str,
        expected_revision: i64,
        listen: bool,
    ) -> Result<InputPolicy> {
        scope.validate()?;
        text(actor, false)?;
        let tx = self
            .con
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mask: Option<i64> = tx.query_row("SELECT permissions FROM route_grants WHERE instance=? AND bot=? AND chat=? AND actor=?", params![scope.instance,scope.bot,scope.chat,actor], |r| r.get(0)).optional()?;
        if mask.is_none_or(|mask| mask & Permission::Manage as i64 == 0) {
            return Err(Error::Unauthorized);
        }
        if input_policy(&tx, scope)?.revision != expected_revision {
            return Err(Error::Conflict);
        }
        let revision = expected_revision.checked_add(1).ok_or(Error::Conflict)?;
        tx.execute("INSERT INTO chat_input_policy(instance,bot,chat,listen,revision) VALUES(?,?,?,?,?) ON CONFLICT(instance,bot,chat) DO UPDATE SET listen=excluded.listen,revision=excluded.revision", params![scope.instance,scope.bot,scope.chat,listen,revision])?;
        tx.commit()?;
        Ok(InputPolicy {
            listen: Some(listen),
            revision,
        })
    }
    pub fn authorize(&self, scope: &Scope, actor: &str, permission: Permission) -> Result<()> {
        scope.validate()?;
        text(actor, false)?;
        let mask:Option<i64>=self.con.query_row("SELECT permissions FROM route_grants WHERE instance=? AND bot=? AND chat=? AND actor=?",params![scope.instance,scope.bot,scope.chat,actor],|r|r.get(0)).optional()?;
        if mask.is_some_and(|mask| mask & permission as i64 != 0) {
            Ok(())
        } else {
            Err(Error::Unauthorized)
        }
    }
    /// One complete successful backend snapshot atomically retires missing
    /// identities and records current ones. Failed/partial discovery cannot retire.
    pub fn inventory(
        &mut self,
        kind: Backend,
        endpoint: &str,
        terminals: &[Terminal],
        complete: bool,
        succeeded: bool,
    ) -> Result<()> {
        text(endpoint, false)?;
        if !succeeded {
            return Ok(());
        }
        let mut seen = HashSet::new();
        let mut panes = HashSet::new();
        for terminal in terminals {
            terminal.validate()?;
            if terminal.identity.backend != kind
                || terminal.identity.endpoint != endpoint
                || !terminal.available
            {
                return Err(Error::Invalid("Inventory scope mismatch"));
            }
            if !seen.insert(terminal.identity.clone())
                || !panes.insert(terminal.identity.pane.clone())
            {
                return Err(Error::Ambiguous);
            }
        }
        let tx = self
            .con
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        if complete {
            tx.execute(
                "UPDATE route_terminals SET available=0 WHERE backend=? AND endpoint=?",
                params![backend(&kind), endpoint],
            )?;
        }
        for terminal in terminals {
            let id = &terminal.identity;
            let identity = encode(id)?;
            let collision:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM route_terminals WHERE backend=? AND endpoint=? AND pane=? AND generation<>? AND available=1)",params![backend(&id.backend),id.endpoint,id.pane,id.generation],|r|r.get(0))?;
            if collision {
                return Err(Error::Ambiguous);
            }
            tx.execute("INSERT INTO route_terminals(identity,backend,endpoint,pane,generation,label,workspace,harness,conversation,available) VALUES(?,?,?,?,?,?,?,?,?,1) ON CONFLICT(identity) DO UPDATE SET label=excluded.label,workspace=excluded.workspace,harness=excluded.harness,conversation=excluded.conversation,available=1",params![identity,backend(&id.backend),id.endpoint,id.pane,id.generation,terminal.label,terminal.workspace,terminal.harness,terminal.conversation])?;
        }
        tx.commit()?;
        Ok(())
    }
    pub fn rename(&mut self, identity: &TerminalId, label: &str) -> Result<()> {
        identity.validate().map_err(Error::Invalid)?;
        text(label, true)?;
        if self.con.execute(
            "UPDATE route_terminals SET label=? WHERE identity=?",
            params![label, encode(identity)?],
        )? != 1
        {
            return Err(Error::Unavailable);
        }
        Ok(())
    }
    pub fn terminal(&self, identity: &TerminalId) -> Result<Option<Terminal>> {
        identity.validate().map_err(Error::Invalid)?;
        Ok(self.con.query_row("SELECT label,workspace,harness,conversation,available FROM route_terminals WHERE identity=?",[encode(identity)?],|r|Ok(Terminal{identity:identity.clone(),label:r.get(0)?,workspace:r.get(1)?,harness:r.get(2)?,conversation:r.get(3)?,available:r.get(4)?})).optional()?)
    }
    fn saved_route(&self, scope: &Scope) -> Result<Option<Route>> {
        read_route(&self.con, scope)
    }
    pub fn inspect(&self, scope: &Scope, actor: &str) -> Result<Option<Route>> {
        self.authorize(scope, actor, Permission::Read)?;
        self.saved_route(scope)
    }
    /// Selection uses an expected revision (zero for no route), preventing a
    /// background task from overwriting a newer operator choice.
    #[allow(clippy::too_many_arguments)]
    pub fn select(
        &mut self,
        scope: &Scope,
        actor: &str,
        target: &TerminalId,
        live: &[TerminalId],
        expected_revision: i64,
        explicit: bool,
        pin: bool,
    ) -> Result<Route> {
        self.authorize(scope, actor, Permission::Manage)?;
        exact(target, live)?;
        let tx = self
            .con
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let current:Option<(i64,bool,String)>=tx.query_row("SELECT revision,pinned,identity FROM chat_routes WHERE instance=? AND bot=? AND chat=?",params![scope.instance,scope.bot,scope.chat],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).optional()?;
        if current.as_ref().map_or(0, |r| r.0) != expected_revision {
            return Err(Error::Conflict);
        }
        let identity = encode(target)?;
        if current.as_ref().is_some_and(|r| r.1 && r.2 != identity) && !explicit {
            return Err(Error::Conflict);
        }
        let available: Option<bool> = tx
            .query_row(
                "SELECT available FROM route_terminals WHERE identity=?",
                [&identity],
                |r| r.get(0),
            )
            .optional()?;
        if available != Some(true) {
            return Err(Error::Unavailable);
        }
        let revision = expected_revision.checked_add(1).ok_or(Error::Conflict)?;
        let pinned = pin || current.is_some_and(|r| r.1);
        tx.execute("INSERT INTO chat_routes(instance,bot,chat,identity,pinned,revision) VALUES(?,?,?,?,?,?) ON CONFLICT(instance,bot,chat) DO UPDATE SET identity=excluded.identity,pinned=excluded.pinned,revision=excluded.revision",params![scope.instance,scope.bot,scope.chat,identity,pinned,revision])?;
        tx.commit()?;
        Ok(Route {
            scope: scope.clone(),
            terminal: target.clone(),
            pinned,
            revision,
        })
    }
    pub fn resolve_send(&self, scope: &Scope, actor: &str, live: &[TerminalId]) -> Result<Route> {
        self.authorize(scope, actor, Permission::Send)?;
        let route = self.saved_route(scope)?.ok_or(Error::Unavailable)?;
        if !self.terminal(&route.terminal)?.is_some_and(|t| t.available) {
            return Err(Error::Unavailable);
        }
        exact(&route.terminal, live)?;
        Ok(route)
    }
    /// Change only the pin policy, preserving exact terminal binding.
    pub fn set_pinned(
        &mut self,
        scope: &Scope,
        actor: &str,
        expected_revision: i64,
        pinned: bool,
    ) -> Result<i64> {
        self.authorize(scope, actor, Permission::Manage)?;
        let revision = expected_revision.checked_add(1).ok_or(Error::Conflict)?;
        let changed = self.con.execute("UPDATE chat_routes SET pinned=?,revision=? WHERE instance=? AND bot=? AND chat=? AND revision=?",params![pinned,revision,scope.instance,scope.bot,scope.chat,expected_revision])?;
        if changed != 1 {
            return Err(Error::Conflict);
        }
        Ok(revision)
    }
    /// Imported routes are staged only. Activation uses normal authorized
    /// `select`; import never copies permission grants or trusts labels as IDs.
    pub fn stage_import(&mut self, preview: &ImportPreview) -> Result<usize> {
        if preview.routes.len() > 10_000 {
            return Err(Error::Invalid("Too many legacy routes"));
        }
        let tx = self
            .con
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut added = 0;
        for item in &preview.routes {
            item.scope.validate()?;
            if let Some(target) = &item.candidate {
                target.validate().map_err(Error::Invalid)?;
            }
            let encoded = encode(item)?;
            let old: Option<String> = tx
                .query_row(
                    "SELECT plan FROM route_imports WHERE instance=? AND bot=? AND chat=?",
                    params![item.scope.instance, item.scope.bot, item.scope.chat],
                    |r| r.get(0),
                )
                .optional()?;
            if let Some(old) = old {
                if decode::<ImportRoute>(&old)? != *item {
                    return Err(Error::Conflict);
                }
                continue;
            }
            tx.execute(
                "INSERT INTO route_imports(instance,bot,chat,plan) VALUES(?,?,?,?)",
                params![
                    item.scope.instance,
                    item.scope.bot,
                    item.scope.chat,
                    encoded
                ],
            )?;
            added += 1;
        }
        tx.commit()?;
        Ok(added)
    }
    pub fn staged(&self, scope: &Scope) -> Result<Option<ImportRoute>> {
        scope.validate()?;
        let saved: Option<String> = self
            .con
            .query_row(
                "SELECT plan FROM route_imports WHERE instance=? AND bot=? AND chat=?",
                params![scope.instance, scope.bot, scope.chat],
                |r| r.get(0),
            )
            .optional()?;
        saved.map(|s| decode(&s)).transpose()
    }
}

/// Local administration snapshot without schema creation or migrations. Remote
/// callers must use Registry::inspect so their grant is checked first.
pub fn read_route(con: &Connection, scope: &Scope) -> Result<Option<Route>> {
    scope.validate()?;
    let real_table: bool = con.query_row(
        "SELECT EXISTS(SELECT 1 FROM main.sqlite_schema WHERE name='chat_routes' AND type='table')",
        [],
        |row| row.get(0),
    )?;
    if !real_table {
        return Err(Error::Invalid("Missing route table"));
    }
    let row:Option<(String,bool,i64)>=con.query_row("SELECT identity,pinned,revision FROM main.chat_routes WHERE instance=? AND bot=? AND chat=?",params![scope.instance,scope.bot,scope.chat],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).optional()?;
    row.map(|(identity, pinned, revision)| {
        Ok(Route {
            scope: scope.clone(),
            terminal: decode(&identity)?,
            pinned,
            revision,
        })
    })
    .transpose()
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LegacyBinding {
    pub agent_id: i64,
    pub backend: String,
    pub server: String,
    pub terminal_id: String,
    pub pane_id: String,
    pub display: String,
}
/// Explicit verified translations supplied by a migration adapter. In particular
/// old tmux PID strings cannot reconstruct a socket path or process start time.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Translation {
    pub legacy: LegacyBinding,
    pub current: TerminalId,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImportRoute {
    pub scope: Scope,
    pub display: String,
    pub candidate: Option<TerminalId>,
    pub pinned: bool,
    pub reason: Option<String>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImportPreview {
    pub routes: Vec<ImportRoute>,
    pub ignored_sections: Vec<String>,
}

/// Allowlisted query against a caller-owned migration snapshot, never an implicit
/// live Armonia path. No scripts, aliases, account data or organization policy read.
pub fn legacy_snapshot(snapshot: &Connection) -> Result<Vec<LegacyBinding>> {
    let mut statement=snapshot.prepare("SELECT b.agent_id,b.backend,b.server,b.terminal_id,b.pane_id,a.display FROM bindings b JOIN agents a ON a.id=b.agent_id ORDER BY b.agent_id LIMIT 10001")?;
    let rows = statement
        .query_map([], |r| {
            Ok(LegacyBinding {
                agent_id: r.get(0)?,
                backend: r.get(1)?,
                server: r.get(2)?,
                terminal_id: r.get(3)?,
                pane_id: r.get(4)?,
                display: r.get(5)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    if rows.len() > 10_000 {
        return Err(Error::Invalid("Legacy registry exceeds import limit"));
    }
    Ok(rows)
}
fn field<'a>(value: &'a Value, key: &str) -> Option<&'a str> {
    value.get(key).and_then(Value::as_str)
}
/// Unknown/custom sections are listed but never executed or copied as policy.
/// The jobs snapshot is bounded and only active/chat_pins/chat_bindings are read.
pub fn preview_import(
    bytes: &[u8],
    instance: &str,
    bot: &str,
    legacy: &[LegacyBinding],
    translations: &[Translation],
    live: &[TerminalId],
) -> Result<ImportPreview> {
    if bytes.len() > 8 * 1024 * 1024 {
        return Err(Error::Invalid("Legacy jobs snapshot exceeds 8 MiB"));
    }
    text(instance, false)?;
    text(bot, false)?;
    let root: Value =
        serde_json::from_slice(bytes).map_err(|_| Error::Invalid("Invalid legacy jobs JSON"))?;
    let root = root
        .as_object()
        .ok_or(Error::Invalid("Legacy jobs must be an object"))?;
    let active = root.get("active").and_then(Value::as_object);
    let meta = root.get("meta").and_then(Value::as_object);
    let pins = meta
        .and_then(|v| v.get("chat_pins"))
        .and_then(Value::as_object);
    let bindings = meta
        .and_then(|v| v.get("chat_bindings"))
        .and_then(Value::as_object);
    let mut chats = BTreeSet::new();
    for mapping in [active, pins, bindings].into_iter().flatten() {
        chats.extend(mapping.keys().cloned());
    }
    if chats.len() > 10_000 {
        return Err(Error::Invalid("Too many legacy routes"));
    }
    let mut routes = Vec::new();
    for chat in chats {
        let scope = Scope {
            instance: instance.into(),
            bot: bot.into(),
            chat: chat.clone(),
        };
        scope.validate()?;
        let target = pins
            .and_then(|p| p.get(&chat))
            .or_else(|| active.and_then(|a| a.get(&chat)))
            .and_then(Value::as_str)
            .unwrap_or("");
        text(target, true)?;
        let saved = bindings.and_then(|b| b.get(&chat));
        let matches: Vec<_> = legacy
            .iter()
            .filter(|row| {
                saved.is_some_and(|b| {
                    b.get("agent_id").and_then(Value::as_i64) == Some(row.agent_id)
                        && field(b, "backend") == Some(row.backend.as_str())
                        && field(b, "server") == Some(row.server.as_str())
                        && field(b, "terminal_id") == Some(row.terminal_id.as_str())
                })
            })
            .collect();
        let mut candidate = None;
        let reason;
        if let [row] = matches.as_slice() {
            // A changed selected name is an explicit unresolved edit, not license
            // to guess alias equivalence. The operator can rebind in the wizard.
            if target != saved.and_then(|b| field(b, "display")).unwrap_or("") {
                reason = Some(
                    "Legacy selection changed after its saved binding; rebind explicitly".into(),
                );
            } else {
                let exact_translations: Vec<_> =
                    translations.iter().filter(|t| &t.legacy == *row).collect();
                let proposed = if let [translation] = exact_translations.as_slice() {
                    Some(translation.current.clone())
                } else if exact_translations.is_empty() && row.backend == "herdr" {
                    Some(TerminalId {
                        backend: Backend::Herdr,
                        endpoint: row.server.clone(),
                        pane: row.pane_id.clone(),
                        generation: row.terminal_id.clone(),
                    })
                } else {
                    None
                };
                match proposed {
                    Some(id) if exact(&id, live).is_ok() => {
                        candidate = Some(id);
                        reason = None;
                    }
                    _ => {
                        reason = Some(
                            "Exact live identity unavailable or ambiguous; rebind explicitly"
                                .into(),
                        );
                    }
                }
            }
        } else {
            reason = Some(
                "No unique saved terminal binding; display names cannot restore routes".into(),
            );
        }
        routes.push(ImportRoute {
            scope,
            display: target.into(),
            candidate,
            pinned: pins.is_some_and(|p| p.contains_key(&chat)),
            reason,
        });
    }
    let mut ignored_sections: Vec<_> = root
        .keys()
        .filter(|key| !matches!(key.as_str(), "active" | "meta"))
        .cloned()
        .collect();
    if let Some(meta) = meta {
        ignored_sections.extend(
            meta.keys()
                .filter(|key| !matches!(key.as_str(), "chat_pins" | "chat_bindings"))
                .map(|key| format!("meta.{key}")),
        );
    }
    Ok(ImportPreview {
        routes,
        ignored_sections,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn readonly_route_rejects_views_in_place_of_authoritative_table() {
        let con = Connection::open_in_memory().unwrap();
        con.execute_batch("CREATE VIEW chat_routes AS SELECT '{}' AS identity,0 AS pinned,1 AS revision,'instance' AS instance,'bot' AS bot,'chat' AS chat;").unwrap();
        assert_eq!(
            read_route(&con, &scope()),
            Err(Error::Invalid("Missing route table"))
        );
    }
    use serde_json::json;
    fn scope() -> Scope {
        Scope {
            instance: "example".into(),
            bot: "123".into(),
            chat: "-100".into(),
        }
    }
    fn terminal() -> Terminal {
        Terminal {
            identity: TerminalId {
                backend: Backend::Herdr,
                endpoint: "project".into(),
                pane: "workspace:pane".into(),
                generation: "terminal-uuid".into(),
            },
            label: "agent".into(),
            workspace: "project".into(),
            harness: "claude".into(),
            conversation: Some("conversation-1".into()),
            available: true,
        }
    }
    fn prepared() -> Registry {
        let mut registry = Registry::memory().unwrap();
        registry
            .set_grant(
                &scope(),
                "owner",
                &[Permission::Read, Permission::Send, Permission::Manage],
            )
            .unwrap();
        registry
            .inventory(Backend::Herdr, "project", &[terminal()], true, true)
            .unwrap();
        registry
    }
    fn select(registry: &mut Registry) -> Route {
        let t = terminal();
        registry
            .select(
                &scope(),
                "owner",
                &t.identity,
                std::slice::from_ref(&t.identity),
                0,
                true,
                true,
            )
            .unwrap()
    }
    fn legacy() -> LegacyBinding {
        LegacyBinding {
            agent_id: 1,
            backend: "herdr".into(),
            server: "project".into(),
            terminal_id: "terminal-uuid".into(),
            pane_id: "workspace:pane".into(),
            display: "agent-1".into(),
        }
    }
    fn jobs() -> Value {
        json!({"active":{"-100":"agent-1"},"meta":{"chat_pins":{"-100":"agent-1"},"chat_bindings":{"-100":{"agent_id":1,"backend":"herdr","server":"project","terminal_id":"terminal-uuid","display":"agent-1"}}},"jobs":{"custom":"not imported"}})
    }
    fn preview(value: &Value, live: &[TerminalId]) -> ImportPreview {
        preview_import(
            &serde_json::to_vec(value).unwrap(),
            "example",
            "123",
            &[legacy()],
            &[],
            live,
        )
        .unwrap()
    }
    #[test]
    fn renamed_labels_never_change_route_or_grants() {
        let mut registry = prepared();
        let route = select(&mut registry);
        registry.rename(&route.terminal, "different-name").unwrap();
        assert_eq!(
            registry
                .resolve_send(&scope(), "owner", std::slice::from_ref(&route.terminal))
                .unwrap(),
            route
        );
        assert_eq!(
            registry.resolve_send(&scope(), "different-name", &[route.terminal]),
            Err(Error::Unauthorized)
        );
    }
    #[test]
    fn grants_are_scoped_by_instance_bot_chat_and_actor() {
        let mut registry = prepared();
        select(&mut registry);
        for changed in [
            Scope {
                instance: "other".into(),
                ..scope()
            },
            Scope {
                bot: "other".into(),
                ..scope()
            },
            Scope {
                chat: "other".into(),
                ..scope()
            },
        ] {
            assert_eq!(
                registry.resolve_send(&changed, "owner", &[terminal().identity]),
                Err(Error::Unauthorized)
            );
        }
        registry
            .set_grant(&scope(), "viewer", &[Permission::Read])
            .unwrap();
        assert!(registry.inspect(&scope(), "viewer").unwrap().is_some());
        assert_eq!(
            registry.resolve_send(&scope(), "viewer", &[terminal().identity]),
            Err(Error::Unauthorized)
        );
        registry.set_grant(&scope(), "owner", &[]).unwrap();
        assert_eq!(
            registry.inspect(&scope(), "owner"),
            Err(Error::Unauthorized)
        );
    }
    #[test]
    fn reused_pane_does_not_restore_old_route() {
        let mut registry = prepared();
        let route = select(&mut registry);
        let mut changed = terminal();
        changed.identity.generation = "new-terminal".into();
        registry
            .inventory(Backend::Herdr, "project", &[changed.clone()], true, true)
            .unwrap();
        assert_eq!(
            registry.resolve_send(&scope(), "owner", &[changed.identity]),
            Err(Error::Unavailable)
        );
        assert_eq!(registry.inspect(&scope(), "owner").unwrap(), Some(route));
    }
    #[test]
    fn failed_or_partial_discovery_cannot_retire_existing_terminal() {
        let mut registry = prepared();
        registry
            .inventory(Backend::Herdr, "project", &[], true, false)
            .unwrap();
        assert!(
            registry
                .terminal(&terminal().identity)
                .unwrap()
                .unwrap()
                .available
        );
        registry
            .inventory(Backend::Herdr, "project", &[], false, true)
            .unwrap();
        assert!(
            registry
                .terminal(&terminal().identity)
                .unwrap()
                .unwrap()
                .available
        );
        registry
            .inventory(Backend::Herdr, "project", &[], true, true)
            .unwrap();
        assert!(
            !registry
                .terminal(&terminal().identity)
                .unwrap()
                .unwrap()
                .available
        );
    }
    #[test]
    fn stale_revision_and_implicit_pinned_changes_are_rejected() {
        let mut registry = prepared();
        select(&mut registry);
        let mut second = terminal();
        second.identity.pane = "other:pane".into();
        second.identity.generation = "other-uuid".into();
        registry
            .inventory(
                Backend::Herdr,
                "project",
                &[terminal(), second.clone()],
                true,
                true,
            )
            .unwrap();
        assert_eq!(
            registry.select(
                &scope(),
                "owner",
                &second.identity,
                &[second.identity.clone()],
                0,
                true,
                true
            ),
            Err(Error::Conflict)
        );
        assert_eq!(
            registry.select(
                &scope(),
                "owner",
                &second.identity,
                &[second.identity.clone()],
                1,
                false,
                true
            ),
            Err(Error::Conflict)
        );
        assert_eq!(
            registry
                .select(
                    &scope(),
                    "owner",
                    &second.identity,
                    &[second.identity.clone()],
                    1,
                    true,
                    true
                )
                .unwrap()
                .revision,
            2
        );
    }
    #[test]
    fn duplicate_identity_snapshot_rejected_atomically() {
        let mut registry = prepared();
        let before = registry.terminal(&terminal().identity).unwrap();
        assert_eq!(
            registry.inventory(
                Backend::Herdr,
                "project",
                &[terminal(), terminal()],
                true,
                true
            ),
            Err(Error::Ambiguous)
        );
        assert_eq!(registry.terminal(&terminal().identity).unwrap(), before);
    }

    #[test]
    fn pin_changes_require_manage_permission_and_current_revision() {
        let mut registry = prepared();
        select(&mut registry);
        registry
            .set_grant(&scope(), "viewer", &[Permission::Read])
            .unwrap();
        assert_eq!(
            registry.set_pinned(&scope(), "viewer", 1, false),
            Err(Error::Unauthorized)
        );
        assert_eq!(
            registry.set_pinned(&scope(), "owner", 0, false),
            Err(Error::Conflict)
        );
        assert_eq!(registry.set_pinned(&scope(), "owner", 1, false).unwrap(), 2);
        assert!(!registry.inspect(&scope(), "owner").unwrap().unwrap().pinned);
    }
    #[test]
    fn exact_legacy_import_is_previewed_staged_and_never_grants_access() {
        let plan = preview(&jobs(), &[terminal().identity]);
        assert_eq!(plan.routes[0].candidate, Some(terminal().identity));
        assert_eq!(plan.ignored_sections, vec!["jobs"]);
        let mut registry = Registry::memory().unwrap();
        assert_eq!(registry.stage_import(&plan).unwrap(), 1);
        assert_eq!(registry.stage_import(&plan).unwrap(), 0);
        assert_eq!(
            registry.staged(&scope()).unwrap(),
            Some(plan.routes[0].clone())
        );
        assert_eq!(
            registry.inspect(&scope(), "owner"),
            Err(Error::Unauthorized)
        );
        registry
            .set_grant(&scope(), "owner", &[Permission::Read])
            .unwrap();
        assert_eq!(registry.inspect(&scope(), "owner").unwrap(), None);
    }
    #[test]
    fn missing_stale_ambiguous_or_renamed_legacy_references_remain_unresolved() {
        assert!(preview(&jobs(), &[]).routes[0].candidate.is_none());
        assert!(
            preview(&jobs(), &[terminal().identity, terminal().identity]).routes[0]
                .candidate
                .is_none()
        );
        let mut changed = jobs();
        changed["meta"]["chat_pins"]["-100"] = json!("changed");
        assert!(
            preview(&changed, &[terminal().identity]).routes[0]
                .candidate
                .is_none()
        );
        changed["meta"]["chat_bindings"] = json!({});
        assert!(
            preview(&changed, &[terminal().identity]).routes[0]
                .candidate
                .is_none()
        );
    }
    #[test]
    fn legacy_tmux_requires_verified_translation_not_pid_or_name_guessing() {
        let mut row = legacy();
        row.backend = "tmux".into();
        row.server = "tmux:123:$1".into();
        row.terminal_id = "tmux:123:$1:%1:456".into();
        row.pane_id = "%1".into();
        let mut value = jobs();
        value["meta"]["chat_bindings"]["-100"]["backend"] = json!(row.backend);
        value["meta"]["chat_bindings"]["-100"]["server"] = json!(row.server);
        value["meta"]["chat_bindings"]["-100"]["terminal_id"] = json!(row.terminal_id);
        let target = TerminalId {
            backend: Backend::Tmux,
            endpoint: crate::test_support::path("/tmp/project.sock").into(),
            pane: "%1".into(),
            generation: "process-generation".into(),
        };
        let data = serde_json::to_vec(&value).unwrap();
        let rows = [row.clone()];
        assert!(
            preview_import(
                &data,
                "example",
                "123",
                &rows,
                &[],
                std::slice::from_ref(&target)
            )
            .unwrap()
            .routes[0]
                .candidate
                .is_none()
        );
        let translation = Translation {
            legacy: row,
            current: target.clone(),
        };
        assert_eq!(
            preview_import(
                &data,
                "example",
                "123",
                &rows,
                &[translation],
                std::slice::from_ref(&target)
            )
            .unwrap()
            .routes[0]
                .candidate,
            Some(target)
        );
    }
    #[test]
    fn legacy_snapshot_reads_only_allowlisted_binding_fields() {
        let source = Connection::open_in_memory().unwrap();
        source.execute_batch("CREATE TABLE agents(id INTEGER,display TEXT,private_data TEXT); CREATE TABLE bindings(agent_id INTEGER,backend TEXT,server TEXT,terminal_id TEXT,pane_id TEXT); INSERT INTO agents VALUES(1,'agent-1','secret must not enter snapshot'); INSERT INTO bindings VALUES(1,'herdr','project','terminal-uuid','workspace:pane');").unwrap();
        assert_eq!(legacy_snapshot(&source).unwrap(), vec![legacy()]);
    }
    #[test]
    fn chat_input_policy_is_scoped_authorized_and_revision_checked() {
        let mut registry = Registry::memory().unwrap();
        let scope = Scope {
            instance: "fixture".into(),
            bot: "123".into(),
            chat: "-42".into(),
        };
        assert_eq!(
            registry.input_policy(&scope).unwrap(),
            InputPolicy {
                listen: None,
                revision: 0
            }
        );
        assert_eq!(
            registry
                .set_input_policy(&scope, "actor", 0, true)
                .unwrap_err(),
            Error::Unauthorized
        );
        registry
            .set_grant(&scope, "actor", &[Permission::Read, Permission::Manage])
            .unwrap();
        assert_eq!(
            registry
                .set_input_policy(&scope, "actor", 0, true)
                .unwrap()
                .revision,
            1
        );
        assert_eq!(
            registry
                .set_input_policy(&scope, "actor", 0, false)
                .unwrap_err(),
            Error::Conflict
        );
        let other = Scope {
            chat: "-43".into(),
            ..scope.clone()
        };
        assert_eq!(registry.input_policy(&other).unwrap().listen, None);
        registry
            .set_grant(&scope, "actor", &[Permission::Read])
            .unwrap();
        assert_eq!(
            registry
                .set_input_policy(&scope, "actor", 1, false)
                .unwrap_err(),
            Error::Unauthorized
        );
        let mut file = Connection::open_in_memory().unwrap();
        rusqlite::backup::Backup::new(&registry.con, &mut file)
            .unwrap()
            .run_to_completion(5, std::time::Duration::from_millis(1), None)
            .unwrap();
        let restored = Registry::from_connection(file).unwrap();
        assert_eq!(
            restored.input_policy(&scope).unwrap(),
            InputPolicy {
                listen: Some(true),
                revision: 1
            }
        );
    }
}
