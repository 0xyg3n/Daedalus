//! Durable goal policy and hook reply receipts. The caller owns a private SQLite
//! connection exclusively and authenticates hook emitters before calling here.
//! No IPC authentication, terminal execution or model request is implemented here.
use super::{
    goals::{Binding, Goal, Status, legacy_directive},
    identity::TerminalId,
};
use crate::app::hooks::{self, GoalOutput, Harness};
use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

pub type Result<T> = std::result::Result<T, Error>;
#[derive(Debug, PartialEq, Eq)]
pub enum Error {
    Database,
    Invalid(&'static str),
    Missing,
    WrongScope,
    Conflict,
    Policy,
    Unsupported,
}
impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Database => "Goal database operation failed",
            Self::Invalid(s) => s,
            Self::Missing => "Goal does not exist",
            Self::WrongScope => "Hook does not belong to this goal",
            Self::Conflict => "Conflicting goal or hook event",
            Self::Policy => "Goal transition is not permitted",
            Self::Unsupported => "Goal hooks are not verified for this harness",
        })
    }
}
impl std::error::Error for Error {}
impl From<rusqlite::Error> for Error {
    fn from(_: rusqlite::Error) -> Self {
        Self::Database
    }
}
fn encode<T: Serialize>(value: &T) -> Result<String> {
    serde_json::to_string(value).map_err(|_| Error::Invalid("Cannot encode goal state"))
}
fn decode<T: serde::de::DeserializeOwned>(value: &str) -> Result<T> {
    serde_json::from_str(value).map_err(|_| Error::Invalid("Invalid stored goal state"))
}
fn identifier(value: &str) -> Result<()> {
    if value.is_empty()
        || value.len() > 512
        || value.chars().any(|c| c.is_control() || c.is_whitespace())
    {
        Err(Error::Invalid("Invalid goal identifier"))
    } else {
        Ok(())
    }
}
fn sql_time(now: u64) -> Result<i64> {
    i64::try_from(now).map_err(|_| Error::Invalid("Goal timestamp exceeds storage range"))
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Scope {
    pub instance: String,
    pub terminal: TerminalId,
    pub conversation: String,
    pub harness: Harness,
}
impl Scope {
    pub fn validate(&self) -> Result<()> {
        identifier(&self.instance)?;
        identifier(&self.conversation)?;
        self.terminal.validate().map_err(Error::Invalid)
    }
    pub fn binding(&self) -> Result<Binding> {
        self.validate()?;
        Ok(Binding {
            terminal: encode(&self.terminal)?,
            conversation: self.conversation.clone(),
        })
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum Reply {
    Empty,
    Json(Value),
    WaitUntil(u64),
    Blocked(String),
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Receipt {
    pub goal_id: String,
    pub event_id: String,
    pub sequence: u64,
    pub reply: Reply,
    pub replayed: bool,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoredGoal {
    pub goal_id: String,
    pub scope: Scope,
    pub goal: Goal,
    pub epoch: i64,
    pub next_sequence: i64,
    pub provenance: Option<Value>,
}
#[derive(Clone, Copy, Debug)]
pub enum Control {
    Pause,
    Complete,
    Resume,
}
const SCHEMA:&str="
CREATE TABLE IF NOT EXISTS goal_records(instance TEXT NOT NULL,goal_id TEXT NOT NULL,scope TEXT NOT NULL,goal TEXT NOT NULL,epoch INTEGER NOT NULL DEFAULT 0,next_sequence INTEGER NOT NULL DEFAULT 0,provenance TEXT,PRIMARY KEY(instance,goal_id));
CREATE TABLE IF NOT EXISTS goal_heads(scope TEXT PRIMARY KEY,instance TEXT NOT NULL,goal_id TEXT NOT NULL,FOREIGN KEY(instance,goal_id) REFERENCES goal_records(instance,goal_id));
CREATE TABLE IF NOT EXISTS goal_events(instance TEXT NOT NULL,event_id TEXT NOT NULL,goal_id TEXT NOT NULL,scope TEXT NOT NULL,payload_sha TEXT NOT NULL,sequence INTEGER NOT NULL,epoch INTEGER NOT NULL,reply TEXT NOT NULL,created_at INTEGER NOT NULL,PRIMARY KEY(instance,event_id),FOREIGN KEY(instance,goal_id) REFERENCES goal_records(instance,goal_id));
CREATE TABLE IF NOT EXISTS goal_activity_events(instance TEXT NOT NULL,event_id TEXT NOT NULL,goal_id TEXT NOT NULL,scope TEXT NOT NULL,payload_sha TEXT NOT NULL,epoch INTEGER NOT NULL,PRIMARY KEY(instance,event_id));
CREATE TABLE IF NOT EXISTS goal_native_turns(instance TEXT NOT NULL,goal_id TEXT NOT NULL,sequence INTEGER NOT NULL,epoch INTEGER NOT NULL,turn_id TEXT NOT NULL,PRIMARY KEY(instance,goal_id,sequence,epoch));
CREATE TABLE IF NOT EXISTS goal_controls(id INTEGER PRIMARY KEY AUTOINCREMENT,instance TEXT NOT NULL,goal_id TEXT NOT NULL,epoch INTEGER NOT NULL,action TEXT NOT NULL,occurred_at INTEGER NOT NULL);
";
fn load(con: &Connection, instance: &str, goal_id: &str) -> Result<StoredGoal> {
    let row:Option<(String,String,i64,i64,Option<String>)>=con.query_row("SELECT scope,goal,epoch,next_sequence,provenance FROM goal_records WHERE instance=? AND goal_id=?",params![instance,goal_id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?))).optional()?;
    let (scope, goal, epoch, next_sequence, provenance) = row.ok_or(Error::Missing)?;
    Ok(StoredGoal {
        goal_id: goal_id.into(),
        scope: decode(&scope)?,
        goal: decode(&goal)?,
        epoch,
        next_sequence,
        provenance: provenance.map(|s| decode(&s)).transpose()?,
    })
}
fn scoped(con: &Connection, goal_id: &str, scope: &Scope) -> Result<StoredGoal> {
    scope.validate()?;
    identifier(goal_id)?;
    let record = load(con, &scope.instance, goal_id)?;
    if record.scope != *scope {
        return Err(Error::WrongScope);
    }
    Ok(record)
}
fn save(tx: &Transaction<'_>, record: &StoredGoal) -> Result<()> {
    tx.execute(
        "UPDATE goal_records SET goal=?,epoch=?,next_sequence=? WHERE instance=? AND goal_id=?",
        params![
            encode(&record.goal)?,
            record.epoch,
            record.next_sequence,
            record.scope.instance,
            record.goal_id
        ],
    )?;
    Ok(())
}
fn audit(tx: &Transaction<'_>, record: &StoredGoal, action: &str, now: u64) -> Result<()> {
    tx.execute(
        "INSERT INTO goal_controls(instance,goal_id,epoch,action,occurred_at) VALUES(?,?,?,?,?)",
        params![
            record.scope.instance,
            record.goal_id,
            record.epoch,
            action,
            sql_time(now)?
        ],
    )?;
    Ok(())
}
fn authoritative_reply(record: &StoredGoal) -> Option<Reply> {
    match &record.goal.status {
        Status::Paused | Status::Completed => Some(Reply::Empty),
        Status::Blocked { reason } => Some(Reply::Blocked(reason.clone())),
        _ => None,
    }
}

pub struct Store {
    con: Connection,
}
impl Store {
    /// Opening the sole state owner recovers uncertain pending/running goals.
    /// Do not open a second writer as a read-only status client.
    pub fn from_connection(mut con: Connection, now: u64) -> Result<Self> {
        sql_time(now)?;
        con.busy_timeout(std::time::Duration::from_secs(5))?;
        con.execute_batch("PRAGMA foreign_keys=ON; PRAGMA synchronous=FULL;")?;
        let tx = con.transaction_with_behavior(TransactionBehavior::Immediate)?;
        tx.execute_batch(SCHEMA)?;
        tx.commit()?;
        let mut store = Self { con };
        store.recover(now)?;
        Ok(store)
    }
    /// Short-lived local hook clients attach without declaring a daemon restart.
    /// SQLite serializes their transitions; the actual service owner alone calls
    /// `from_connection`/`recover` when recovering from its own restart.
    pub fn attach_connection(mut con: Connection) -> Result<Self> {
        con.busy_timeout(std::time::Duration::from_secs(5))?;
        con.execute_batch("PRAGMA foreign_keys=ON; PRAGMA synchronous=FULL;")?;
        let tx = con.transaction_with_behavior(TransactionBehavior::Immediate)?;
        tx.execute_batch(SCHEMA)?;
        tx.commit()?;
        Ok(Self { con })
    }
    pub fn memory() -> Result<Self> {
        Self::from_connection(Connection::open_in_memory()?, 0)
    }
    /// Close only after stopping hook intake; callers may reopen for recovery.
    pub fn into_connection(self) -> Connection {
        self.con
    }
    pub fn get(&self, goal_id: &str, scope: &Scope) -> Result<StoredGoal> {
        scoped(&self.con, goal_id, scope)
    }
    pub fn current_continuation(&self, goal_id: &str, scope: &Scope) -> Result<Option<String>> {
        let record = scoped(&self.con, goal_id, scope)?;
        if !matches!(
            record.goal.status,
            Status::Pending { .. } | Status::Running { .. }
        ) {
            return Ok(None);
        }
        Ok(self.con.query_row("SELECT event_id FROM goal_events WHERE instance=? AND goal_id=? AND sequence=? AND epoch=?", params![scope.instance, goal_id, record.goal.last_sequence as i64, record.epoch], |row| row.get(0)).optional()?)
    }
    /// Exact persisted continuation text, only while that continuation remains
    /// current. Lets native prompt observations follow a goal-created turn.
    pub fn continuation_prompt(&self, goal_id: &str, scope: &Scope) -> Result<Option<String>> {
        let Some(event) = self.current_continuation(goal_id, scope)? else {
            return Ok(None);
        };
        let value: String = self.con.query_row(
            "SELECT reply FROM goal_events WHERE instance=? AND event_id=?",
            params![scope.instance, event],
            |r| r.get(0),
        )?;
        match decode::<Reply>(&value)? {
            Reply::Json(value) => Ok(value
                .get("reason")
                .or_else(|| value.get("followup_message"))
                .or_else(|| value.pointer("/entries/0/content"))
                .and_then(Value::as_str)
                .map(str::to_owned)),
            _ => Ok(None),
        }
    }
    /// Status views use this operation so expired persisted leases cannot appear
    /// running simply because another hook has not arrived yet.
    pub fn inspect(
        &mut self,
        goal_id: &str,
        scope: &Scope,
        now: u64,
        pending_timeout_ms: u64,
    ) -> Result<StoredGoal> {
        sql_time(now)?;
        if pending_timeout_ms == 0 {
            return Err(Error::Invalid("Pending goal timeout must be positive"));
        }
        let tx = self
            .con
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut record = scoped(&tx, goal_id, scope)?;
        let previous = record.goal.status.clone();
        record.goal.expire(now, pending_timeout_ms);
        if previous != record.goal.status {
            record.epoch = record.epoch.checked_add(1).ok_or(Error::Conflict)?;
            save(&tx, &record)?;
            audit(&tx, &record, "expired", now)?;
        }
        tx.commit()?;
        Ok(record)
    }
    pub fn arm(
        &mut self,
        goal_id: &str,
        scope: &Scope,
        directive: &str,
        now: u64,
    ) -> Result<StoredGoal> {
        self.create(goal_id, scope, directive, None, false, now)
    }
    fn create(
        &mut self,
        goal_id: &str,
        scope: &Scope,
        directive: &str,
        provenance: Option<Value>,
        paused: bool,
        now: u64,
    ) -> Result<StoredGoal> {
        identifier(goal_id)?;
        scope.validate()?;
        sql_time(now)?;
        if !matches!(
            scope.harness,
            Harness::Claude | Harness::Codex | Harness::Cursor | Harness::Pi | Harness::Grok
        ) {
            return Err(Error::Unsupported);
        }
        let mut goal = Goal::arm(directive, scope.binding()?)
            .map_err(|_| Error::Invalid("Invalid goal directive"))?;
        if paused {
            goal.pause();
        }
        let tx = self
            .con
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let existing: Option<String> = tx
            .query_row(
                "SELECT goal_id FROM goal_heads WHERE scope=?",
                [encode(scope)?],
                |r| r.get(0),
            )
            .optional()?;
        if let Some(existing) = existing {
            let previous = load(&tx, &scope.instance, &existing)?;
            if previous.goal.status != Status::Completed {
                return Err(Error::Conflict);
            }
        }
        let record = StoredGoal {
            goal_id: goal_id.into(),
            scope: scope.clone(),
            goal,
            epoch: 0,
            next_sequence: 0,
            provenance,
        };
        tx.execute("INSERT INTO goal_records(instance,goal_id,scope,goal,epoch,next_sequence,provenance) VALUES(?,?,?,?,0,0,?)",params![scope.instance,goal_id,encode(scope)?,encode(&record.goal)?,record.provenance.as_ref().map(encode).transpose()?])?;
        tx.execute("INSERT INTO goal_heads(scope,instance,goal_id) VALUES(?,?,?) ON CONFLICT(scope) DO UPDATE SET instance=excluded.instance,goal_id=excluded.goal_id",params![encode(scope)?,scope.instance,goal_id])?;
        audit(
            &tx,
            &record,
            if paused { "import_paused" } else { "arm" },
            now,
        )?;
        tx.commit()?;
        Ok(record)
    }
    /// Never reads flags from disk. Explicitly selected legacy text is retained
    /// with a content hash and source name, paused until a separate resume action.
    pub fn import_flag(
        &mut self,
        goal_id: &str,
        scope: &Scope,
        flag_name: &str,
        contents: &str,
        opt_in: bool,
        now: u64,
    ) -> Result<Option<StoredGoal>> {
        if !opt_in {
            return Err(Error::Policy);
        }
        if flag_name != "goal"
            && (!flag_name.starts_with("goal.")
                || flag_name.len() <= 5
                || flag_name.contains('/')
                || flag_name.contains('\\'))
        {
            return Err(Error::Invalid("Invalid legacy goal flag name"));
        }
        identifier(flag_name)?;
        let Some(directive) = legacy_directive(contents) else {
            return Ok(None);
        };
        let provenance = serde_json::json!({"source":"legacy_goal_flag","flag":flag_name,"content_sha256":format!("{:x}",Sha256::digest(contents.as_bytes())),"imported_at":now,"activation":"explicit_resume_required"});
        match self.get(goal_id, scope) {
            Ok(existing) => {
                if existing.goal.directive == directive
                    && existing.provenance.as_ref().is_some_and(|previous| {
                        previous["flag"] == provenance["flag"]
                            && previous["content_sha256"] == provenance["content_sha256"]
                            && previous["source"] == provenance["source"]
                    })
                {
                    return Ok(Some(existing));
                }
                return Err(Error::Conflict);
            }
            Err(Error::Missing) => (),
            Err(error) => return Err(error),
        }
        self.create(goal_id, scope, directive, Some(provenance), true, now)
            .map(Some)
    }
    pub fn control(
        &mut self,
        goal_id: &str,
        scope: &Scope,
        action: Control,
        now: u64,
    ) -> Result<StoredGoal> {
        self.control_inner(goal_id, scope, None, action, now)
    }
    /// Apply a reviewed control only while the durable lifecycle epoch matches.
    /// Scope and epoch are checked under the same write transaction as mutation.
    pub fn control_expected(
        &mut self,
        goal_id: &str,
        scope: &Scope,
        expected_epoch: i64,
        action: Control,
        now: u64,
    ) -> Result<StoredGoal> {
        self.control_inner(goal_id, scope, Some(expected_epoch), action, now)
    }
    fn control_inner(
        &mut self,
        goal_id: &str,
        scope: &Scope,
        expected_epoch: Option<i64>,
        action: Control,
        now: u64,
    ) -> Result<StoredGoal> {
        sql_time(now)?;
        let tx = self
            .con
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut record = scoped(&tx, goal_id, scope)?;
        if expected_epoch.is_some_and(|epoch| epoch != record.epoch) {
            return Err(Error::Conflict);
        }
        match action {
            Control::Pause => record.goal.pause(),
            Control::Complete => record.goal.complete(),
            Control::Resume => record
                .goal
                .resume(&scope.binding()?)
                .map_err(|_| Error::Policy)?,
        }
        record.epoch = record.epoch.checked_add(1).ok_or(Error::Conflict)?;
        save(&tx, &record)?;
        audit(
            &tx,
            &record,
            match action {
                Control::Pause => "pause",
                Control::Complete => "complete",
                Control::Resume => "resume",
            },
            now,
        )?;
        tx.commit()?;
        Ok(record)
    }
    /// Input identity is parsed against separately authenticated scope. Event IDs
    /// must be stable hook invocation IDs assigned/verified by the service.
    /// The returned reply is committed before the transport may write it.
    pub fn stop(
        &mut self,
        goal_id: &str,
        scope: &Scope,
        event_id: &str,
        payload: &[u8],
        now: u64,
        pending_timeout_ms: u64,
    ) -> Result<Receipt> {
        identifier(event_id)?;
        sql_time(now)?;
        let binding = scope.binding()?;
        let event = hooks::parse(scope.harness, payload, &binding)
            .map_err(|_| Error::Invalid("Invalid goal hook payload"))?;
        if event.kind != hooks::EventKind::Stop {
            return Err(Error::Invalid("Goal continuation requires a Stop event"));
        }
        if !matches!(
            scope.harness,
            Harness::Claude | Harness::Codex | Harness::Cursor | Harness::Pi | Harness::Grok
        ) {
            return Err(Error::Unsupported);
        }
        if scope.harness == Harness::Grok && !event.grok_turn_gate {
            return Err(Error::Unsupported);
        }
        if pending_timeout_ms == 0 {
            return Err(Error::Invalid("Pending goal timeout must be positive"));
        }
        let canonical: Value = serde_json::from_slice(payload)
            .map_err(|_| Error::Invalid("Invalid goal hook payload"))?;
        let sha = format!(
            "{:x}",
            Sha256::digest(encode(&(scope, &canonical))?.as_bytes())
        );
        let tx = self
            .con
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut record = scoped(&tx, goal_id, scope)?;
        let old_status = record.goal.status.clone();
        record.goal.expire(now, pending_timeout_ms);
        if record.goal.status != old_status {
            record.epoch = record.epoch.checked_add(1).ok_or(Error::Conflict)?;
            audit(&tx, &record, "expired", now)?;
        }
        type Existing = (String, String, i64, i64, String);
        let old:Option<Existing>=tx.query_row("SELECT goal_id,payload_sha,sequence,epoch,reply FROM goal_events WHERE instance=? AND event_id=?",params![scope.instance,event_id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?))).optional()?;
        let (sequence, replayed, old_reply) =
            if let Some((previous_goal, previous_hash, sequence, epoch, reply)) = old {
                if previous_goal != goal_id || previous_hash != sha {
                    return Err(Error::Conflict);
                }
                let reply: Reply = decode(&reply)?;
                if epoch != record.epoch {
                    let reply = authoritative_reply(&record).unwrap_or_else(|| {
                        Reply::Blocked(
                            "Hook event predates a goal lifecycle change; do not replay".into(),
                        )
                    });
                    save(&tx, &record)?;
                    tx.commit()?;
                    return Ok(Receipt {
                        goal_id: goal_id.into(),
                        event_id: event_id.into(),
                        sequence: sequence as u64,
                        reply,
                        replayed: true,
                    });
                }
                (sequence, true, Some(reply))
            } else {
                record.next_sequence =
                    record.next_sequence.checked_add(1).ok_or(Error::Conflict)?;
                (record.next_sequence, false, None)
            };
        let reply = if let Some(reply) = authoritative_reply(&record) {
            reply
        } else if matches!(old_reply, Some(Reply::Blocked(_) | Reply::Empty)) {
            old_reply.ok_or(Error::Conflict)?
        } else {
            match hooks::goal_response(&event, Some(&mut record.goal), sequence as u64, now)
                .map_err(|_| Error::Policy)?
            {
                GoalOutput::Empty => Reply::Empty,
                GoalOutput::Json(value) => Reply::Json(value),
                GoalOutput::WaitUntil(deadline) => Reply::WaitUntil(deadline),
                GoalOutput::Blocked(reason) => Reply::Blocked(reason),
                GoalOutput::ReplayReceipt => old_reply.ok_or(Error::Conflict)?,
            }
        };
        if scope.harness == Harness::Grok && matches!(reply, Reply::Json(_)) {
            let turn = canonical
                .get("promptId")
                .and_then(Value::as_str)
                .ok_or(Error::Policy)?;
            tx.execute("INSERT OR IGNORE INTO goal_native_turns(instance,goal_id,sequence,epoch,turn_id) VALUES(?,?,?,?,?)",params![scope.instance,goal_id,sequence,record.epoch,turn])?;
        }
        save(&tx, &record)?;
        tx.execute("INSERT INTO goal_events(instance,event_id,goal_id,scope,payload_sha,sequence,epoch,reply,created_at) VALUES(?,?,?,?,?,?,?,?,?) ON CONFLICT(instance,event_id) DO UPDATE SET reply=excluded.reply",params![scope.instance,event_id,goal_id,encode(scope)?,sha,sequence,record.epoch,encode(&reply)?,sql_time(now)?])?;
        tx.commit()?;
        Ok(Receipt {
            goal_id: goal_id.into(),
            event_id: event_id.into(),
            sequence: sequence as u64,
            reply,
            replayed,
        })
    }
    /// Activity must correlate with the stored continuation event; neither a
    /// successful write nor a callback containing an arbitrary sequence is proof.
    pub fn activity(
        &mut self,
        goal_id: &str,
        scope: &Scope,
        event_id: &str,
        now: u64,
        lease_ms: u64,
        pending_timeout_ms: u64,
    ) -> Result<StoredGoal> {
        identifier(event_id)?;
        sql_time(now)?;
        if pending_timeout_ms == 0 {
            return Err(Error::Invalid("Pending goal timeout must be positive"));
        }
        let tx = self
            .con
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut record = scoped(&tx, goal_id, scope)?;
        let previous = record.goal.status.clone();
        record.goal.expire(now, pending_timeout_ms);
        if previous != record.goal.status {
            record.epoch = record.epoch.checked_add(1).ok_or(Error::Conflict)?;
            save(&tx, &record)?;
            audit(&tx, &record, "expired", now)?;
            tx.commit()?;
            return Err(Error::Policy);
        }
        let event:Option<(String,i64,i64,String)>=tx.query_row("SELECT goal_id,sequence,epoch,reply FROM goal_events WHERE instance=? AND event_id=?",params![scope.instance,event_id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?))).optional()?;
        let (id, sequence, epoch, reply) = event.ok_or(Error::Missing)?;
        if id != goal_id
            || epoch != record.epoch
            || !matches!(decode::<Reply>(&reply)?, Reply::Json(_))
        {
            return Err(Error::Conflict);
        }
        record
            .goal
            .acknowledge_activity(&scope.binding()?, sequence as u64, now, lease_ms)
            .map_err(|_| Error::Policy)?;
        save(&tx, &record)?;
        tx.commit()?;
        Ok(record)
    }
    /// Record a verified lifecycle observation exactly once, atomically with any
    /// activity lease update. Old observations never renew a later continuation.
    #[allow(clippy::too_many_arguments)]
    pub fn observe_activity(
        &mut self,
        goal_id: &str,
        scope: &Scope,
        event_id: &str,
        payload: &[u8],
        now: u64,
        lease_ms: u64,
        pending_timeout_ms: u64,
    ) -> Result<()> {
        identifier(event_id)?;
        sql_time(now)?;
        if lease_ms == 0 || pending_timeout_ms == 0 {
            return Err(Error::Policy);
        }
        let value: Value = serde_json::from_slice(payload).map_err(|_| Error::Policy)?;
        let sha = format!("{:x}", Sha256::digest(encode(&value)?.as_bytes()));
        let tx = self
            .con
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut record = scoped(&tx, goal_id, scope)?;
        let old: Option<(String,String,String)> = tx.query_row("SELECT goal_id,scope,payload_sha FROM goal_activity_events WHERE instance=? AND event_id=?",params![scope.instance,event_id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).optional()?;
        if let Some((id, binding, hash)) = old {
            if id != goal_id || decode::<Scope>(&binding)? != *scope || hash != sha {
                return Err(Error::Conflict);
            }
            return Ok(());
        }
        let before = record.goal.status.clone();
        record.goal.expire(now, pending_timeout_ms);
        if before != record.goal.status {
            record.epoch = record.epoch.checked_add(1).ok_or(Error::Conflict)?;
            audit(&tx, &record, "expired", now)?;
        }
        if matches!(
            record.goal.status,
            Status::Pending { .. } | Status::Running { .. }
        ) {
            let continuation: Option<String> = tx.query_row("SELECT event_id FROM goal_events WHERE instance=? AND goal_id=? AND sequence=? AND epoch=?",params![scope.instance,goal_id,sql_time(record.goal.last_sequence)?,record.epoch],|r|r.get(0)).optional()?;
            let grok_correlated = if scope.harness == Harness::Grok {
                let expected: Option<String> = tx.query_row("SELECT turn_id FROM goal_native_turns WHERE instance=? AND goal_id=? AND sequence=? AND epoch=?",params![scope.instance,goal_id,sql_time(record.goal.last_sequence)?,record.epoch],|row| row.get(0)).optional()?;
                let native = value.get("promptId").and_then(Value::as_str);
                let event = hooks::parse(scope.harness, payload, &scope.binding()?)
                    .map_err(|_| Error::Policy)?;
                expected.as_deref().is_some_and(|turn| Some(turn) == native)
                    && value.get("subagentType").is_none_or(Value::is_null)
                    && (event.kind == hooks::EventKind::PostToolUse
                        || (event.grok_turn_gate && event.stop_hook_active))
            } else {
                true
            };
            if grok_correlated && continuation.as_deref().is_some_and(|id| id != event_id) {
                record
                    .goal
                    .acknowledge_activity(
                        &scope.binding()?,
                        record.goal.last_sequence,
                        now,
                        lease_ms,
                    )
                    .map_err(|_| Error::Policy)?;
            }
        }
        save(&tx, &record)?;
        tx.execute("INSERT INTO goal_activity_events(instance,event_id,goal_id,scope,payload_sha,epoch) VALUES(?,?,?,?,?,?)",params![scope.instance,event_id,goal_id,encode(scope)?,sha,record.epoch])?;
        tx.commit()?;
        Ok(())
    }
    pub fn recover(&mut self, now: u64) -> Result<usize> {
        sql_time(now)?;
        let tx = self
            .con
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let ids = tx
            .prepare("SELECT instance,goal_id FROM goal_heads")?
            .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let mut count = 0;
        for (instance, id) in ids {
            let mut record = load(&tx, &instance, &id)?;
            let old = record.goal.status.clone();
            record.goal.recover();
            if old != record.goal.status {
                record.epoch = record.epoch.checked_add(1).ok_or(Error::Conflict)?;
                save(&tx, &record)?;
                audit(&tx, &record, "recovered", now)?;
                count += 1;
            }
        }
        tx.commit()?;
        Ok(count)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::identity::Backend;
    use serde_json::json;
    fn scope() -> Scope {
        Scope {
            instance: "test".into(),
            terminal: TerminalId {
                backend: Backend::Herdr,
                endpoint: "project".into(),
                pane: "workspace:pane".into(),
                generation: "terminal-1".into(),
            },
            conversation: "session-1".into(),
            harness: Harness::Claude,
        }
    }
    fn payload() -> Vec<u8> {
        serde_json::to_vec(&json!({"session_id":"session-1","hook_event_name":"Stop"})).unwrap()
    }
    fn prepared() -> Store {
        let mut store = Store::memory().unwrap();
        store
            .arm("goal-1", &scope(), "Complete verified work", 0)
            .unwrap();
        store
    }
    fn stop(store: &mut Store, id: &str, now: u64) -> Receipt {
        store
            .stop("goal-1", &scope(), id, &payload(), now, 30_000)
            .unwrap()
    }
    #[test]
    fn reviewed_control_rejects_stale_epoch_without_mutation() {
        let mut store = prepared();
        let original = store.get("goal-1", &scope()).unwrap();
        let paused = store
            .control_expected("goal-1", &scope(), original.epoch, Control::Pause, 1)
            .unwrap();
        assert_eq!(paused.epoch, original.epoch + 1);
        assert!(matches!(
            store.control_expected("goal-1", &scope(), original.epoch, Control::Complete, 2),
            Err(Error::Conflict)
        ));
        let unchanged = store.get("goal-1", &scope()).unwrap();
        assert_eq!(unchanged, paused);
        let resumed = store
            .control_expected("goal-1", &scope(), paused.epoch, Control::Resume, 3)
            .unwrap();
        assert_eq!(resumed.goal.status, Status::Armed);
        assert!(matches!(
            store.control_expected("goal-1", &scope(), paused.epoch, Control::Complete, 4),
            Err(Error::Conflict)
        ));
    }
    #[test]
    fn transition_receipt_sequence_and_duplicate_are_atomic() {
        let mut store = prepared();
        let original = stop(&mut store, "event-1", 0);
        assert_eq!(original.sequence, 1);
        assert!(!original.replayed);
        assert!(matches!(original.reply, Reply::Json(_)));
        let duplicate = stop(&mut store, "event-1", 1);
        assert!(duplicate.replayed);
        assert_eq!(duplicate.reply, original.reply);
        assert_eq!(duplicate.sequence, original.sequence);
        let current = store.get("goal-1", &scope()).unwrap();
        assert_eq!(current.next_sequence, 1);
        assert!(matches!(current.goal.status, Status::Pending { .. }));
    }
    #[test]
    fn failed_receipt_write_rolls_back_goal_transition_and_sequence() {
        let mut store = prepared();
        store.con.execute_batch("CREATE TRIGGER fail_event BEFORE INSERT ON goal_events BEGIN SELECT RAISE(ABORT,'test'); END;").unwrap();
        assert!(
            store
                .stop("goal-1", &scope(), "event-1", &payload(), 0, 30_000)
                .is_err()
        );
        let current = store.get("goal-1", &scope()).unwrap();
        assert_eq!(current.goal.status, Status::Armed);
        assert_eq!(current.next_sequence, 0);
        assert_eq!(
            store
                .con
                .query_row("SELECT count(*) FROM goal_events", [], |r| r
                    .get::<_, i64>(0))
                .unwrap(),
            0
        );
    }
    #[test]
    fn pause_complete_and_recovery_override_old_continue_receipts() {
        for action in [Control::Pause, Control::Complete] {
            let mut store = prepared();
            stop(&mut store, "event-1", 0);
            store.control("goal-1", &scope(), action, 1).unwrap();
            assert_eq!(stop(&mut store, "event-1", 2).reply, Reply::Empty);
        }
        let mut store = prepared();
        stop(&mut store, "event-1", 0);
        assert_eq!(store.recover(1).unwrap(), 1);
        assert!(matches!(
            stop(&mut store, "event-1", 2).reply,
            Reply::Blocked(_)
        ));
    }
    #[test]
    fn resume_never_replays_pre_pause_or_pre_recovery_events() {
        let mut store = prepared();
        stop(&mut store, "event-1", 0);
        store
            .control("goal-1", &scope(), Control::Pause, 1)
            .unwrap();
        store
            .control("goal-1", &scope(), Control::Resume, 2)
            .unwrap();
        assert!(matches!(
            stop(&mut store, "event-1", 3).reply,
            Reply::Blocked(_)
        ));
        assert!(matches!(
            stop(&mut store, "event-2", 8_000).reply,
            Reply::Json(_)
        ));
        store.recover(8_001).unwrap();
        store
            .control("goal-1", &scope(), Control::Resume, 8_002)
            .unwrap();
        assert!(matches!(
            stop(&mut store, "event-2", 8_003).reply,
            Reply::Blocked(_)
        ));
    }
    #[test]
    fn conflicting_event_id_and_wrong_binding_make_no_writes() {
        let mut store = prepared();
        stop(&mut store, "event-1", 0);
        let before = store.get("goal-1", &scope()).unwrap();
        let changed = serde_json::to_vec(
            &json!({"session_id":"session-1","hook_event_name":"Stop","extra":"changed"}),
        )
        .unwrap();
        assert_eq!(
            store.stop("goal-1", &scope(), "event-1", &changed, 1, 30_000),
            Err(Error::Conflict)
        );
        let mut wrong = scope();
        wrong.terminal.generation = "other".into();
        assert_eq!(
            store.stop("goal-1", &wrong, "event-2", &payload(), 1, 30_000),
            Err(Error::WrongScope)
        );
        assert_eq!(store.get("goal-1", &scope()).unwrap(), before);
    }
    #[test]
    fn cooldown_retry_uses_same_sequence_and_release_wins() {
        let mut store = prepared();
        stop(&mut store, "event-1", 0);
        store
            .activity("goal-1", &scope(), "event-1", 1, 30_000, 30_000)
            .unwrap();
        let waiting = stop(&mut store, "event-2", 2);
        assert_eq!(waiting.sequence, 2);
        assert_eq!(waiting.reply, Reply::WaitUntil(8_000));
        let continued = stop(&mut store, "event-2", 8_000);
        assert_eq!(continued.sequence, 2);
        assert!(continued.replayed);
        assert!(matches!(continued.reply, Reply::Json(_)));
        store
            .control("goal-1", &scope(), Control::Complete, 8_001)
            .unwrap();
        assert_eq!(stop(&mut store, "event-2", 8_002).reply, Reply::Empty);
    }
    #[test]
    fn stale_activity_cannot_revive_expired_or_paused_execution() {
        let mut store = prepared();
        stop(&mut store, "event-1", 0);
        store
            .activity("goal-1", &scope(), "event-1", 1, 10, 30_000)
            .unwrap();
        assert_eq!(
            store.activity("goal-1", &scope(), "event-1", 11, 10, 30_000),
            Err(Error::Policy)
        );
        assert!(matches!(
            store.get("goal-1", &scope()).unwrap().goal.status,
            Status::Blocked { .. }
        ));
        let mut store = prepared();
        stop(&mut store, "event-1", 0);
        store
            .control("goal-1", &scope(), Control::Pause, 1)
            .unwrap();
        assert_eq!(
            store.activity("goal-1", &scope(), "event-1", 2, 10, 30_000),
            Err(Error::Conflict)
        );
    }
    #[test]
    fn legacy_import_requires_opt_in_and_explicit_resume_with_provenance() {
        let mut store = Store::memory().unwrap();
        assert_eq!(
            store.import_flag("goal-1", &scope(), "goal.window", "Finish work", false, 0),
            Err(Error::Policy)
        );
        assert!(
            store
                .import_flag("released", &scope(), "goal.window", "done", true, 0)
                .unwrap()
                .is_none()
        );
        let imported = store
            .import_flag("goal-1", &scope(), "goal.window", "Finish work", true, 0)
            .unwrap()
            .unwrap();
        assert_eq!(imported.goal.status, Status::Paused);
        assert_eq!(
            imported.provenance.unwrap()["activation"],
            "explicit_resume_required"
        );
        assert_eq!(stop(&mut store, "event-1", 1).reply, Reply::Empty);
        store
            .control("goal-1", &scope(), Control::Resume, 2)
            .unwrap();
        assert!(matches!(
            stop(&mut store, "event-1", 3).reply,
            Reply::Blocked(_)
        ));
        assert!(matches!(
            stop(&mut store, "event-2", 4).reply,
            Reply::Json(_)
        ));
    }
    #[test]
    fn reopened_owner_recovers_pending_state_without_automatic_rearm() {
        let mut store = prepared();
        stop(&mut store, "event-1", 0);
        let connection = store.into_connection();
        let mut reopened = Store::from_connection(connection, 1).unwrap();
        assert!(matches!(
            reopened.get("goal-1", &scope()).unwrap().goal.status,
            Status::Blocked { .. }
        ));
        assert!(matches!(
            stop(&mut reopened, "event-1", 2).reply,
            Reply::Blocked(_)
        ));
        assert_eq!(reopened.get("goal-1", &scope()).unwrap().next_sequence, 1);
    }
    #[test]
    fn completed_goal_can_be_replaced_but_event_ids_cannot_cross_goals() {
        let mut store = prepared();
        stop(&mut store, "event-1", 0);
        assert!(store.arm("goal-2", &scope(), "Different work", 1).is_err());
        store
            .control("goal-1", &scope(), Control::Complete, 2)
            .unwrap();
        store.arm("goal-2", &scope(), "Different work", 3).unwrap();
        assert_eq!(
            store.stop("goal-2", &scope(), "event-1", &payload(), 4, 30_000),
            Err(Error::Conflict)
        );
        assert!(matches!(
            store
                .stop("goal-2", &scope(), "event-2", &payload(), 4, 30_000)
                .unwrap()
                .reply,
            Reply::Json(_)
        ));
    }

    #[test]
    fn status_inspection_expires_leases_without_waiting_for_another_hook() {
        let mut store = prepared();
        stop(&mut store, "event-1", 0);
        store
            .activity("goal-1", &scope(), "event-1", 1, 10, 30_000)
            .unwrap();
        assert!(matches!(
            store
                .inspect("goal-1", &scope(), 11, 30_000)
                .unwrap()
                .goal
                .status,
            Status::Blocked { .. }
        ));
        assert!(matches!(
            stop(&mut store, "event-1", 12).reply,
            Reply::Blocked(_)
        ));
    }

    #[test]
    fn repeated_import_preserves_completion_instead_of_rearming() {
        let mut store = Store::memory().unwrap();
        store
            .import_flag("goal-1", &scope(), "goal", "Finish work", true, 0)
            .unwrap();
        store
            .control("goal-1", &scope(), Control::Complete, 1)
            .unwrap();
        let imported = store
            .import_flag("goal-1", &scope(), "goal", "Finish work", true, 2)
            .unwrap()
            .unwrap();
        assert_eq!(imported.goal.status, Status::Completed);
        assert_eq!(
            store.import_flag("goal-1", &scope(), "goal", "Changed work", true, 3),
            Err(Error::Conflict)
        );
    }

    #[cfg(unix)]
    #[test]
    fn closed_database_preserves_uncertain_reply_for_reconciliation() {
        use std::os::unix::fs::DirBuilderExt;
        use std::sync::atomic::{AtomicU64, Ordering};
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let directory = std::env::temp_dir().join(format!(
            "daedalus-goal-store-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&directory)
            .unwrap();
        struct Temp(std::path::PathBuf);
        impl Drop for Temp {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
        let cleanup = Temp(directory);
        let path = cleanup.0.join("goals.sqlite3");
        let mut store = Store::from_connection(Connection::open(&path).unwrap(), 0).unwrap();
        store
            .arm("goal-1", &scope(), "Complete verified work", 0)
            .unwrap();
        let issued = stop(&mut store, "event-1", 0);
        assert!(matches!(issued.reply, Reply::Json(_)));
        store.into_connection().close().unwrap();
        let mut reopened = Store::from_connection(Connection::open(&path).unwrap(), 1).unwrap();
        assert!(matches!(
            stop(&mut reopened, "event-1", 2).reply,
            Reply::Blocked(_)
        ));
        let original: String = reopened
            .con
            .query_row(
                "SELECT reply FROM goal_events WHERE event_id='event-1'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert!(matches!(
            decode::<Reply>(&original).unwrap(),
            Reply::Json(_)
        ));
    }
}
