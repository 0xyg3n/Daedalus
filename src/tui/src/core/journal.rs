//! Durable delivery state compatible with the Python delivery journal schema.
//! No transport calls occur here. An expired intent is uncertain, never retried.
use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    fmt,
    io::{BufRead, BufReader, Read},
    path::Path,
    time::Duration,
};

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS delivery_priority(delivery_id TEXT PRIMARY KEY,priority INTEGER NOT NULL);
CREATE TABLE IF NOT EXISTS delivery_retry(delivery_id TEXT PRIMARY KEY,next_at REAL NOT NULL);
CREATE TABLE IF NOT EXISTS bursts(id INTEGER PRIMARY KEY AUTOINCREMENT,burst_key TEXT NOT NULL,state TEXT NOT NULL,first_at REAL NOT NULL,last_at REAL NOT NULL,delivery_id TEXT);
CREATE UNIQUE INDEX IF NOT EXISTS burst_open_key ON bursts(burst_key) WHERE state='open';
CREATE TABLE IF NOT EXISTS burst_items(id INTEGER PRIMARY KEY AUTOINCREMENT,burst_id INTEGER NOT NULL REFERENCES bursts(id),event_key TEXT NOT NULL UNIQUE,item TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS callbacks(bot TEXT NOT NULL,token TEXT NOT NULL UNIQUE,data TEXT NOT NULL,UNIQUE(bot,data));
CREATE TABLE IF NOT EXISTS inbox(id INTEGER PRIMARY KEY AUTOINCREMENT,channel TEXT NOT NULL,bot TEXT NOT NULL,event_id TEXT NOT NULL,payload TEXT NOT NULL,state TEXT NOT NULL,created_at REAL NOT NULL,error TEXT,UNIQUE(channel,bot,event_id));
CREATE TABLE IF NOT EXISTS cursors(channel TEXT NOT NULL,bot TEXT NOT NULL,offset INTEGER NOT NULL,PRIMARY KEY(channel,bot));
CREATE TABLE IF NOT EXISTS progress_heads(binding TEXT PRIMARY KEY,event_id INTEGER NOT NULL);
CREATE TABLE IF NOT EXISTS delivery(id INTEGER PRIMARY KEY AUTOINCREMENT,delivery_id TEXT NOT NULL UNIQUE,direction TEXT NOT NULL,destination TEXT NOT NULL,payload TEXT NOT NULL,payload_sha TEXT NOT NULL,origin TEXT NOT NULL,dedup_key TEXT NOT NULL,state TEXT NOT NULL,created_at REAL NOT NULL,worker TEXT,lease_token TEXT,lease_until REAL,attempts INTEGER NOT NULL DEFAULT 0,receipt TEXT,error TEXT,UNIQUE(direction,dedup_key));
CREATE TABLE IF NOT EXISTS quarantine(source TEXT NOT NULL,line_number INTEGER NOT NULL,raw TEXT NOT NULL,error TEXT,PRIMARY KEY(source,line_number,raw));
CREATE INDEX IF NOT EXISTS delivery_ready ON delivery(direction,destination,state,id);
CREATE INDEX IF NOT EXISTS delivery_active_destinations ON delivery(direction,destination) WHERE state IN ('pending','retryable','intent');
CREATE INDEX IF NOT EXISTS hook_prompt_scope ON inbox(bot,json_extract(payload,'$.scope'),id DESC) WHERE channel='hooks' AND CASE WHEN json_valid(payload) THEN json_extract(payload,'$.event')='UserPromptSubmit' ELSE 0 END;
CREATE INDEX IF NOT EXISTS delivery_expiring_intents ON delivery(lease_until) WHERE state='intent';
CREATE INDEX IF NOT EXISTS inbox_recovery ON inbox(channel,bot,state,id);
";

#[derive(Debug)]
pub enum Error {
    Database(rusqlite::Error),
    Io(std::io::Error),
    Json(serde_json::Error),
    Conflict(&'static str),
    Invalid(&'static str),
}
impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Database(_) => f.write_str("Delivery database operation failed"),
            Self::Io(_) => f.write_str("Private journal file operation failed"),
            Self::Json(_) => f.write_str("Invalid journal JSON"),
            Self::Conflict(s) | Self::Invalid(s) => f.write_str(s),
        }
    }
}
impl std::error::Error for Error {}
impl From<rusqlite::Error> for Error {
    fn from(e: rusqlite::Error) -> Self {
        Self::Database(e)
    }
}
impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}
impl From<serde_json::Error> for Error {
    fn from(e: serde_json::Error) -> Self {
        Self::Json(e)
    }
}
pub type Result<T> = std::result::Result<T, Error>;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Delivery {
    pub id: i64,
    pub delivery_id: String,
    pub direction: String,
    pub destination: String,
    pub payload: Value,
    pub origin: Value,
    pub dedup_key: String,
    pub state: String,
    pub created_at: f64,
    pub worker: Option<String>,
    pub lease_token: Option<String>,
    pub lease_until: Option<f64>,
    pub attempts: i64,
    pub receipt: Option<Value>,
    pub error: Option<String>,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Event {
    pub id: i64,
    pub channel: String,
    pub bot: String,
    pub event_id: String,
    pub payload: Value,
    pub state: String,
    pub created_at: f64,
    pub error: Option<String>,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Burst {
    pub id: i64,
    pub burst_key: String,
    pub state: String,
    pub first_at: f64,
    pub last_at: f64,
    pub delivery_id: Option<String>,
    pub items: Vec<Value>,
}
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ImportReport {
    pub imported: usize,
    pub quarantined: usize,
}

fn timestamp(now: f64) -> Result<()> {
    if !now.is_finite() || now < 0.0 {
        Err(Error::Invalid("Invalid journal timestamp"))
    } else {
        Ok(())
    }
}
fn direction(value: &str) -> Result<()> {
    if matches!(value, "inbound" | "outbound") {
        Ok(())
    } else {
        Err(Error::Invalid("Invalid delivery direction"))
    }
}
fn present(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(v) => *v,
        Value::String(v) => !v.is_empty(),
        Value::Array(v) => !v.is_empty(),
        Value::Object(v) => !v.is_empty(),
        Value::Number(_) => true,
    }
}
fn parse_json(row: &rusqlite::Row<'_>, name: &str) -> rusqlite::Result<Value> {
    let value: String = row.get(name)?;
    serde_json::from_str(&value).map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(e))
    })
}
fn delivery(row: &rusqlite::Row<'_>) -> rusqlite::Result<Delivery> {
    let receipt: Option<String> = row.get("receipt")?;
    let receipt = receipt
        .map(|s| serde_json::from_str(&s))
        .transpose()
        .map_err(|e| {
            rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(e))
        })?;
    Ok(Delivery {
        id: row.get("id")?,
        delivery_id: row.get("delivery_id")?,
        direction: row.get("direction")?,
        destination: row.get("destination")?,
        payload: parse_json(row, "payload")?,
        origin: parse_json(row, "origin")?,
        dedup_key: row.get("dedup_key")?,
        state: row.get("state")?,
        created_at: row.get("created_at")?,
        worker: row.get("worker")?,
        lease_token: row.get("lease_token")?,
        lease_until: row.get("lease_until")?,
        attempts: row.get("attempts")?,
        receipt,
        error: row.get("error")?,
    })
}
fn token(con: &Connection) -> Result<String> {
    Ok(con.query_row("SELECT lower(hex(randomblob(16)))", [], |r| r.get(0))?)
}
fn admit_tx(
    tx: &Transaction<'_>,
    channel: &str,
    bot: &str,
    event_id: &str,
    payload: &Value,
    now: f64,
) -> Result<bool> {
    let old: Option<String> = tx
        .query_row(
            "SELECT payload FROM inbox WHERE channel=? AND bot=? AND event_id=?",
            params![channel, bot, event_id],
            |r| r.get(0),
        )
        .optional()?;
    if let Some(old) = old {
        if serde_json::from_str::<Value>(&old)? != *payload {
            return Err(Error::Conflict("Event ID reused with different payload"));
        }
        return Ok(false);
    }
    tx.execute("INSERT INTO inbox(channel,bot,event_id,payload,state,created_at) VALUES(?,?,?,?,'admitted',?)",params![channel,bot,event_id,serde_json::to_string(payload)?,now])?;
    Ok(true)
}
fn advance_tx(tx: &Transaction<'_>, channel: &str, bot: &str, offset: i64) -> Result<()> {
    let previous = offset
        .checked_sub(1)
        .ok_or(Error::Invalid("Invalid cursor offset"))?;
    let exists: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM inbox WHERE channel=? AND bot=? AND event_id=?)",
        params![channel, bot, previous.to_string()],
        |r| r.get(0),
    )?;
    if !exists {
        return Err(Error::Conflict("Cannot acknowledge an unadmitted update"));
    }
    tx.execute("INSERT INTO cursors(channel,bot,offset) VALUES(?,?,?) ON CONFLICT(channel,bot) DO UPDATE SET offset=MAX(offset,excluded.offset)",params![channel,bot,offset])?;
    Ok(())
}

/// One connection; mutation methods require exclusive access. The service should
/// put this owner on its database worker, rather than block the rendering thread.
pub struct Journal {
    con: Connection,
}
impl Journal {
    #[cfg(unix)]
    pub fn open(path: &Path) -> Result<Self> {
        use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt};
        if !path.is_absolute()
            || path
                .components()
                .any(|component| matches!(component, std::path::Component::ParentDir))
        {
            return Err(Error::Invalid(
                "Journal path must be absolute without parent traversal",
            ));
        }
        let parent = path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .ok_or(Error::Invalid("Journal needs a private parent directory"))?;
        for component in parent.ancestors() {
            let meta = match std::fs::symlink_metadata(component) {
                Ok(meta) => meta,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                Err(error) => return Err(error.into()),
            };
            if meta.file_type().is_symlink() {
                return Err(Error::Invalid("Journal parent must not contain symlinks"));
            }
        }
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(parent)?;
        let meta = std::fs::metadata(parent)?;
        if meta.uid() != unsafe { libc::geteuid() } || meta.mode() & 0o077 != 0 {
            return Err(Error::Invalid(
                "Journal parent must be private and owned by this user",
            ));
        }
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK)
            .open(path)?;
        let meta = file.metadata()?;
        if !meta.is_file() || meta.uid() != unsafe { libc::geteuid() } || meta.nlink() != 1 {
            return Err(Error::Invalid(
                "Journal must be an owned regular file without hard links",
            ));
        }
        file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
        let flags = rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE
            | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX
            | rusqlite::OpenFlags::SQLITE_OPEN_NOFOLLOW;
        Self::initialize(Connection::open_with_flags(path, flags)?)
    }
    pub fn memory() -> Result<Self> {
        Self::initialize(Connection::open_in_memory()?)
    }
    fn initialize(mut con: Connection) -> Result<Self> {
        con.busy_timeout(Duration::from_secs(5))?;
        con.execute_batch("PRAGMA synchronous=FULL; PRAGMA foreign_keys=ON;")?;
        let tx = con.transaction_with_behavior(TransactionBehavior::Immediate)?;
        tx.execute_batch(SCHEMA)?;
        tx.commit()?;
        Ok(Self { con })
    }
    fn transaction(&mut self) -> Result<Transaction<'_>> {
        Ok(self
            .con
            .transaction_with_behavior(TransactionBehavior::Immediate)?)
    }
    pub fn admit(
        &mut self,
        channel: &str,
        bot: &str,
        event_id: &str,
        payload: &Value,
        now: f64,
    ) -> Result<bool> {
        timestamp(now)?;
        let tx = self.transaction()?;
        let inserted = admit_tx(&tx, channel, bot, event_id, payload, now)?;
        tx.commit()?;
        Ok(inserted)
    }
    /// Admission and acknowledgement are atomic when processing a polling update.
    pub fn admit_and_advance(
        &mut self,
        channel: &str,
        bot: &str,
        event_id: i64,
        payload: &Value,
        now: f64,
    ) -> Result<bool> {
        timestamp(now)?;
        let offset = event_id
            .checked_add(1)
            .ok_or(Error::Invalid("Invalid event ID"))?;
        let tx = self.transaction()?;
        let inserted = admit_tx(&tx, channel, bot, &event_id.to_string(), payload, now)?;
        advance_tx(&tx, channel, bot, offset)?;
        tx.commit()?;
        Ok(inserted)
    }
    pub fn advance_cursor(&mut self, channel: &str, bot: &str, offset: i64) -> Result<()> {
        let tx = self.transaction()?;
        advance_tx(&tx, channel, bot, offset)?;
        tx.commit()?;
        Ok(())
    }
    pub fn cursor(&self, channel: &str, bot: &str, default: i64) -> Result<i64> {
        Ok(self
            .con
            .query_row(
                "SELECT offset FROM cursors WHERE channel=? AND bot=?",
                params![channel, bot],
                |r| r.get(0),
            )
            .optional()?
            .unwrap_or(default))
    }
    pub fn events(&self, channel: &str, bot: &str, states: &[&str]) -> Result<Vec<Event>> {
        self.events_page(channel, bot, states, 0, 10_000)
    }
    /// Read at most `limit` events, continuing after the last returned ID.
    /// Durable start boundary for opt-in hook streaming; no historical replay.
    pub fn hook_head(&self, instance: &str) -> Result<i64> {
        Ok(self.con.query_row(
            "SELECT COALESCE(MAX(id),0) FROM inbox WHERE channel='hooks' AND bot=?",
            [instance],
            |row| row.get(0),
        )?)
    }
    /// Latest real user prompt in one exact registered scope. Unrelated tool
    /// messages cannot reopen an older Telegram turn.
    pub fn latest_hook_prompt(&self, instance: &str, scope: &Value) -> Result<Option<Event>> {
        let encoded = serde_json::to_string(scope)?;
        Ok(self.con.query_row("SELECT * FROM inbox WHERE channel='hooks' AND bot=? AND CASE WHEN json_valid(payload) THEN json_extract(payload,'$.event')='UserPromptSubmit' ELSE 0 END AND json_extract(payload,'$.scope')=json(?) ORDER BY id DESC LIMIT 1",params![instance,encoded],|r|Ok(Event {
            id:r.get("id")?,channel:r.get("channel")?,bot:r.get("bot")?,event_id:r.get("event_id")?,payload:parse_json(r,"payload")?,state:r.get("state")?,created_at:r.get("created_at")?,error:r.get("error")?,
        })).optional()?)
    }
    pub fn events_page(
        &self,
        channel: &str,
        bot: &str,
        states: &[&str],
        after_id: i64,
        limit: usize,
    ) -> Result<Vec<Event>> {
        if limit == 0 || limit > 10_000 || after_id < 0 {
            return Err(Error::Invalid("Invalid inbox pagination"));
        }
        if states.is_empty() {
            return Ok(Vec::new());
        }
        if states.len() > 64 {
            return Err(Error::Invalid("Too many inbox state filters"));
        }
        let placeholders = vec!["?"; states.len()].join(",");
        let sql = format!(
            "SELECT * FROM inbox WHERE channel=? AND bot=? AND state IN ({placeholders}) AND id>{after_id} ORDER BY id LIMIT {limit}"
        );
        let mut stmt = self.con.prepare(&sql)?;
        let bindings = [channel, bot].into_iter().chain(states.iter().copied());
        let rows = stmt.query_map(rusqlite::params_from_iter(bindings), |r| {
            Ok(Event {
                id: r.get("id")?,
                channel: r.get("channel")?,
                bot: r.get("bot")?,
                event_id: r.get("event_id")?,
                payload: parse_json(r, "payload")?,
                state: r.get("state")?,
                created_at: r.get("created_at")?,
                error: r.get("error")?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }
    pub fn event_state(&mut self, id: i64, state: &str, error: Option<&str>) -> Result<()> {
        if !matches!(
            state,
            "processing" | "handled" | "ignored" | "failed" | "unknown"
        ) {
            return Err(Error::Invalid("Invalid inbox state"));
        }
        if self.con.execute(
            "UPDATE inbox SET state=?,error=? WHERE id=?",
            params![state, error, id],
        )? != 1
        {
            return Err(Error::Conflict("Inbox event does not exist"));
        }
        Ok(())
    }
    /// Reserve the newest observed progress state. Equal IDs allow replay of a
    /// partially staged effect; older pending observations cannot regress it.
    pub fn progress_head(&mut self, binding: &str, event: i64) -> Result<bool> {
        if binding.is_empty() || binding.len() > 256 || event <= 0 {
            return Err(Error::Invalid("Invalid progress observation"));
        }
        Ok(self.con.execute("INSERT INTO progress_heads(binding,event_id) VALUES(?,?) ON CONFLICT(binding) DO UPDATE SET event_id=excluded.event_id WHERE excluded.event_id>=progress_heads.event_id",params![binding,event])? == 1)
    }
    pub fn recover_events(&mut self, channel: &str, bot: &str) -> Result<usize> {
        Ok(self.con.execute("UPDATE inbox SET state='unknown',error='process stopped during handler; no automatic control replay' WHERE channel=? AND bot=? AND state='processing'",params![channel,bot])?)
    }
    pub fn enqueue(
        &mut self,
        dir: &str,
        destination: &str,
        payload: &Value,
        dedup_key: &str,
        origin: Option<&Value>,
        now: f64,
    ) -> Result<String> {
        direction(dir)?;
        timestamp(now)?;
        let tx = self.transaction()?;
        let old = tx
            .query_row(
                "SELECT * FROM delivery WHERE direction=? AND dedup_key=?",
                params![dir, dedup_key],
                delivery,
            )
            .optional()?;
        if let Some(old) = old {
            if old.payload != *payload || old.destination != destination {
                return Err(Error::Conflict(
                    "Delivery key reused with different content or destination",
                ));
            }
            return Ok(old.delivery_id);
        }
        let encoded = serde_json::to_string(payload)?;
        let sha = format!("{:x}", Sha256::digest(encoded.as_bytes()));
        let id = token(&tx)?;
        tx.execute("INSERT INTO delivery(delivery_id,direction,destination,payload,payload_sha,origin,dedup_key,state,created_at) VALUES(?,?,?,?,?,?,?,'pending',?)",params![id,dir,destination,encoded,sha,serde_json::to_string(origin.unwrap_or(&json!({})))?,dedup_key,now])?;
        tx.commit()?;
        Ok(id)
    }
    /// Priority then FIFO among eligible rows. Unknown/blocked rows are retained
    /// but do not freeze later messages, matching the existing Python behavior.
    /// A delayed first eligible row holds later eligible rows until its retry time.
    pub fn begin(
        &mut self,
        dir: &str,
        destination: &str,
        worker: &str,
        now: f64,
        lease_seconds: f64,
    ) -> Result<Option<Delivery>> {
        direction(dir)?;
        timestamp(now)?;
        if worker.is_empty()
            || !lease_seconds.is_finite()
            || lease_seconds <= 0.0
            || !(now + lease_seconds).is_finite()
        {
            return Err(Error::Invalid("Invalid delivery lease"));
        }
        let tx = self.transaction()?;
        tx.execute("UPDATE delivery SET state='unknown',error='worker lease expired after intent' WHERE state='intent' AND lease_until<=?",[now])?;
        let active: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM delivery WHERE direction=? AND destination=? AND state='intent')",
            params![dir, destination], |row| row.get(0),
        )?;
        if active {
            tx.commit()?;
            return Ok(None);
        }
        let row=tx.query_row("SELECT * FROM delivery WHERE direction=? AND destination=? AND state IN ('pending','retryable') ORDER BY COALESCE((SELECT priority FROM delivery_priority WHERE delivery_id=delivery.delivery_id),0),id LIMIT 1",params![dir,destination],delivery).optional()?;
        let Some(row) = row else {
            tx.commit()?;
            return Ok(None);
        };
        let next: Option<f64> = tx
            .query_row(
                "SELECT next_at FROM delivery_retry WHERE delivery_id=?",
                [&row.delivery_id],
                |r| r.get(0),
            )
            .optional()?;
        if next.is_some_and(|next| next > now) {
            tx.commit()?;
            return Ok(None);
        }
        let lease = token(&tx)?;
        tx.execute("UPDATE delivery SET state='intent',worker=?,lease_token=?,lease_until=?,attempts=attempts+1 WHERE id=?",params![worker,lease,now+lease_seconds,row.id])?;
        let result = tx.query_row("SELECT * FROM delivery WHERE id=?", [row.id], delivery)?;
        tx.commit()?;
        Ok(Some(result))
    }
    pub fn abandon(&mut self, id: &str, reason: &str) -> Result<()> {
        if reason.trim().is_empty() {
            return Err(Error::Invalid("Abandon requires a reason"));
        }
        if self.con.execute("UPDATE delivery SET state='cancelled',error=? WHERE delivery_id=? AND state IN ('pending','retryable')",params![reason,id])?!=1{return Err(Error::Conflict("Only pending or retryable delivery may be abandoned"));}
        Ok(())
    }
    /// The transport adapter must correlate receipt identifiers with this exact
    /// delivery; this layer validates presence and lease ownership only.
    #[allow(clippy::too_many_arguments)]
    pub fn finish(
        &mut self,
        id: &str,
        lease: &str,
        outcome: &str,
        receipt: Option<&Value>,
        error: Option<&str>,
        retry_after: f64,
        now: f64,
    ) -> Result<()> {
        timestamp(now)?;
        if !retry_after.is_finite() || retry_after < 0.0 || !(now + retry_after).is_finite() {
            return Err(Error::Invalid("Invalid retry delay"));
        }
        let state = match outcome {
            "not_sent" => "retryable",
            "draft_blocked" => "blocked",
            "transport_submitted" => "unknown",
            "accepted" | "delivered" | "unknown" => outcome,
            _ => return Err(Error::Invalid("Invalid transport outcome")),
        };
        if matches!(state, "accepted" | "delivered") && !receipt.is_some_and(present) {
            return Err(Error::Conflict("Completion requires a correlated receipt"));
        }
        let tx = self.transaction()?;
        let row = tx
            .query_row("SELECT * FROM delivery WHERE delivery_id=?", [id], delivery)
            .optional()?
            .ok_or(Error::Conflict("Unknown delivery"))?;
        if row.state != "intent" || row.lease_token.as_deref() != Some(lease) {
            return Err(Error::Conflict("Stale or unowned delivery lease"));
        }
        if row.lease_until.is_none_or(|until| until <= now) {
            tx.execute("UPDATE delivery SET state='unknown',error='worker lease expired after intent' WHERE delivery_id=?",[id])?;
            tx.commit()?;
            return Err(Error::Conflict(
                "Expired delivery lease requires reconciliation",
            ));
        }
        tx.execute(
            "UPDATE delivery SET state=?,receipt=?,error=?,lease_until=NULL WHERE delivery_id=?",
            params![
                state,
                receipt.map(serde_json::to_string).transpose()?,
                error,
                id
            ],
        )?;
        tx.execute("INSERT INTO delivery_retry(delivery_id,next_at) VALUES(?,?) ON CONFLICT(delivery_id) DO UPDATE SET next_at=excluded.next_at",params![id,if retry_after>0.0{now+retry_after}else{0.0}])?;
        tx.commit()?;
        Ok(())
    }
    pub fn checkpoint(&mut self, id: &str, lease: &str, receipt: &Value, now: f64) -> Result<()> {
        timestamp(now)?;
        let tx = self.transaction()?;
        tx.execute("UPDATE delivery SET state='unknown',error='worker lease expired after intent' WHERE delivery_id=? AND state='intent' AND lease_until<=?",params![id,now])?;
        let changed = tx.execute("UPDATE delivery SET receipt=? WHERE delivery_id=? AND state='intent' AND lease_token=? AND lease_until>?",params![serde_json::to_string(receipt)?,id,lease,now])?;
        tx.commit()?;
        if changed != 1 {
            return Err(Error::Conflict(
                "Cannot checkpoint an unowned or expired delivery",
            ));
        }
        Ok(())
    }
    /// A correlated remote receipt remains evidence even if the owner was too
    /// busy to observe it before lease expiry. This never authorizes a retry or
    /// a new submission: only this exact outbound attempt can be reconciled.
    pub fn record_delivery_receipt(
        &mut self,
        id: &str,
        lease: &str,
        receipt: &Value,
        now: f64,
    ) -> Result<()> {
        timestamp(now)?;
        if lease.is_empty() || !present(receipt) {
            return Err(Error::Invalid("Delivery evidence is missing"));
        }
        let tx = self.transaction()?;
        let row = tx
            .query_row("SELECT * FROM delivery WHERE delivery_id=?", [id], delivery)
            .optional()?
            .ok_or(Error::Conflict("Unknown delivery"))?;
        if row.direction != "outbound" || row.lease_token.as_deref() != Some(lease) {
            return Err(Error::Conflict(
                "Receipt does not own this delivery attempt",
            ));
        }
        if row.state == "delivered" && row.receipt.as_ref() == Some(receipt) {
            tx.commit()?;
            return Ok(());
        }
        if !matches!(row.state.as_str(), "intent" | "unknown") {
            return Err(Error::Conflict(
                "Delivery state cannot accept this attempt's receipt",
            ));
        }
        tx.execute("UPDATE delivery SET state='delivered',receipt=?,error=NULL,lease_until=NULL WHERE delivery_id=?",params![serde_json::to_string(receipt)?,id])?;
        tx.execute("DELETE FROM delivery_retry WHERE delivery_id=?", [id])?;
        tx.commit()?;
        Ok(())
    }
    pub fn reconcile(&mut self, id: &str, outcome: &str, evidence: &Value) -> Result<()> {
        if !matches!(
            outcome,
            "accepted" | "delivered" | "retryable" | "cancelled"
        ) || !present(evidence)
        {
            return Err(Error::Invalid("Explicit reconciliation evidence required"));
        }
        if self.con.execute("UPDATE delivery SET state=?,receipt=?,error=NULL,lease_until=NULL WHERE delivery_id=? AND state IN ('unknown','blocked')",params![outcome,serde_json::to_string(evidence)?,id])?!=1{return Err(Error::Conflict("Only ambiguous or blocked delivery may be reconciled"));}
        Ok(())
    }
    pub fn rows(&self) -> Result<Vec<Delivery>> {
        Ok(self
            .con
            .prepare("SELECT * FROM delivery ORDER BY id")?
            .query_map([], delivery)?
            .collect::<rusqlite::Result<_>>()?)
    }
    /// Bounded keyset traversal for service scheduling; never load payloads or
    /// historical receipts just to find destinations. `begin` remains the only
    /// authority for retry time, active leases and priority/FIFO ordering.
    pub fn destinations(&self, dir: &str, after: &str, limit: usize) -> Result<Vec<String>> {
        direction(dir)?;
        if !(1..=1000).contains(&limit) {
            return Err(Error::Invalid("Invalid destination page size"));
        }
        Ok(self.con.prepare(
            "SELECT DISTINCT destination FROM delivery WHERE direction=? AND destination>? AND state IN ('pending','retryable','intent') ORDER BY destination LIMIT ?"
        )?.query_map(params![dir, after, limit as i64], |row| row.get(0))?
            .collect::<rusqlite::Result<_>>()?)
    }
    pub fn delivery_for_key(&self, dir: &str, key: &str) -> Result<Option<Delivery>> {
        Ok(self
            .con
            .query_row(
                "SELECT * FROM delivery WHERE direction=? AND dedup_key=?",
                params![dir, key],
                delivery,
            )
            .optional()?)
    }
    /// A later provider status may settle only the exact accepted submission
    /// the caller observed. It cannot manufacture acceptance or enable a retry.
    pub fn settle_accepted(
        &mut self,
        expected: &Delivery,
        state: &str,
        receipt: &Value,
    ) -> Result<()> {
        if expected.direction != "outbound"
            || expected.state != "accepted"
            || !matches!(state, "accepted" | "delivered" | "blocked" | "cancelled")
            || !present(receipt)
        {
            return Err(Error::Invalid("Invalid accepted-submission observation"));
        }
        let tx = self.transaction()?;
        let current = tx
            .query_row(
                "SELECT * FROM delivery WHERE delivery_id=?",
                [&expected.delivery_id],
                delivery,
            )
            .optional()?;
        if current.as_ref() != Some(expected) {
            return Err(Error::Conflict(
                "Accepted submission changed during observation",
            ));
        }
        tx.execute(
            "UPDATE delivery SET state=?,receipt=?,error=NULL,lease_until=NULL WHERE delivery_id=?",
            params![state, serde_json::to_string(receipt)?, expected.delivery_id],
        )?;
        tx.commit()?;
        Ok(())
    }
    pub fn set_priority(&mut self, id: &str, priority: i64) -> Result<()> {
        let tx = self.transaction()?;
        if !tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM delivery WHERE delivery_id=?)",
            [id],
            |r| r.get::<_, bool>(0),
        )? {
            return Err(Error::Conflict("Unknown priority delivery"));
        }
        tx.execute("INSERT INTO delivery_priority(delivery_id,priority) VALUES(?,?) ON CONFLICT(delivery_id) DO UPDATE SET priority=excluded.priority",params![id,priority])?;
        tx.commit()?;
        Ok(())
    }
    pub fn add_burst(&mut self, key: &str, item: &Value, event_key: &str, now: f64) -> Result<i64> {
        timestamp(now)?;
        let tx = self.transaction()?;
        let old:Option<(i64,String,String)>=tx.query_row("SELECT burst_id,item,burst_key FROM burst_items JOIN bursts ON bursts.id=burst_items.burst_id WHERE event_key=?",[event_key],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).optional()?;
        if let Some((id, old, old_key)) = old {
            if serde_json::from_str::<Value>(&old)? != *item || old_key != key {
                return Err(Error::Conflict("Burst event changed"));
            }
            return Ok(id);
        }
        let existing: Option<i64> = tx
            .query_row(
                "SELECT id FROM bursts WHERE burst_key=? AND state='open'",
                [key],
                |r| r.get(0),
            )
            .optional()?;
        let id = match existing {
            Some(id) => id,
            None => {
                tx.execute(
                    "INSERT INTO bursts(burst_key,state,first_at,last_at) VALUES(?,'open',?,?)",
                    params![key, now, now],
                )?;
                tx.last_insert_rowid()
            }
        };
        tx.execute(
            "INSERT INTO burst_items(burst_id,event_key,item) VALUES(?,?,?)",
            params![id, event_key, serde_json::to_string(item)?],
        )?;
        tx.execute(
            "UPDATE bursts SET last_at=MAX(last_at,?) WHERE id=?",
            params![now, id],
        )?;
        tx.commit()?;
        Ok(id)
    }
    pub fn ready_bursts(
        &mut self,
        quiet: f64,
        maxwait: f64,
        force: bool,
        now: f64,
    ) -> Result<Vec<Burst>> {
        timestamp(now)?;
        if !quiet.is_finite() || !maxwait.is_finite() || quiet < 0.0 || maxwait < 0.0 {
            return Err(Error::Invalid("Invalid burst timing"));
        }
        let tx = self.transaction()?;
        tx.execute("UPDATE bursts SET state='sealed' WHERE state='open' AND (? OR last_at<=? OR first_at<=?)",params![force,now-quiet,now-maxwait])?;
        let mut bursts = tx
            .prepare("SELECT * FROM bursts WHERE state='sealed' ORDER BY id")?
            .query_map([], |r| {
                Ok(Burst {
                    id: r.get("id")?,
                    burst_key: r.get("burst_key")?,
                    state: r.get("state")?,
                    first_at: r.get("first_at")?,
                    last_at: r.get("last_at")?,
                    delivery_id: r.get("delivery_id")?,
                    items: Vec::new(),
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        for burst in &mut bursts {
            burst.items = tx
                .prepare("SELECT item FROM burst_items WHERE burst_id=? ORDER BY id")?
                .query_map([burst.id], |r| parse_json(r, "item"))?
                .collect::<rusqlite::Result<_>>()?;
        }
        tx.commit()?;
        Ok(bursts)
    }
    pub fn finish_burst(&mut self, id: i64, delivery_id: &str) -> Result<()> {
        let tx = self.transaction()?;
        let exists: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM delivery WHERE delivery_id=?)",
            [delivery_id],
            |r| r.get(0),
        )?;
        if !exists {
            return Err(Error::Conflict(
                "Cannot finish burst before durable delivery",
            ));
        }
        if tx.execute("UPDATE bursts SET state='enqueued',delivery_id=? WHERE id=? AND (state='sealed' OR (state='enqueued' AND delivery_id=?))",params![delivery_id,id,delivery_id])?!=1{return Err(Error::Conflict("Burst must be sealed and not rebound"));}
        tx.commit()?;
        Ok(())
    }
    pub fn has_bursts(&self) -> Result<bool> {
        Ok(self.con.query_row(
            "SELECT EXISTS(SELECT 1 FROM bursts WHERE state IN ('open','sealed'))",
            [],
            |r| r.get(0),
        )?)
    }
    pub fn callback_token(&mut self, bot: &str, data: &str) -> Result<String> {
        let tx = self.transaction()?;
        if let Some(value) = tx
            .query_row(
                "SELECT token FROM callbacks WHERE bot=? AND data=?",
                params![bot, data],
                |r| r.get::<_, String>(0),
            )
            .optional()?
        {
            return Ok(value);
        }
        let value = format!("cb:{}", token(&tx)?);
        tx.execute(
            "INSERT INTO callbacks(bot,token,data) VALUES(?,?,?)",
            params![bot, value, data],
        )?;
        tx.commit()?;
        Ok(value)
    }
    pub fn callback_data(&self, bot: &str, token: &str) -> Result<Option<String>> {
        Ok(self
            .con
            .query_row(
                "SELECT data FROM callbacks WHERE bot=? AND token=?",
                params![bot, token],
                |r| r.get(0),
            )
            .optional()?)
    }
    /// Streaming import; originals retained. A line over 1 MiB stops import with
    /// an explicit error; earlier rows remain durable and repeat import is safe.
    pub fn import_jsonl(
        &mut self,
        path: &Path,
        dir: &str,
        destination_key: &str,
        now: f64,
    ) -> Result<ImportReport> {
        direction(dir)?;
        timestamp(now)?;
        let mut options = std::fs::OpenOptions::new();
        options.read(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC);
        }
        let file = match options.open(path) {
            Ok(file) => file,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Ok(ImportReport::default());
            }
            Err(e) => return Err(e.into()),
        };
        if !file.metadata()?.is_file() {
            return Err(Error::Invalid("Legacy journal must be a regular file"));
        }
        let absolute = path.canonicalize()?;
        let absolute = absolute
            .to_str()
            .ok_or(Error::Invalid("Legacy source path must be UTF-8"))?;
        let source = path
            .to_str()
            .ok_or(Error::Invalid("Legacy source path must be UTF-8"))?;
        let mut reader = BufReader::new(file);
        let mut report = ImportReport::default();
        let mut index = 0_usize;
        loop {
            let mut raw = Vec::new();
            let count = (&mut reader)
                .take(1024 * 1024 + 1)
                .read_until(b'\n', &mut raw)?;
            if count == 0 {
                break;
            }
            if count > 1024 * 1024 {
                return Err(Error::Invalid("Legacy journal line exceeds 1 MiB"));
            }
            if raw.last() == Some(&b'\n') {
                raw.pop();
            }
            if raw.last() == Some(&b'\r') {
                raw.pop();
            }
            let line_index = index;
            index += 1;
            if raw.iter().all(u8::is_ascii_whitespace) {
                continue;
            }
            let parsed = serde_json::from_slice::<Value>(&raw);
            if let Ok(payload) = parsed.as_ref()
                && payload.is_object()
            {
                let mut hash = Sha256::new();
                hash.update(format!("{absolute}\0{line_index}\0").as_bytes());
                hash.update(&raw);
                let destination = match payload.get(destination_key) {
                    Some(Value::String(s)) => s.clone(),
                    Some(value) => value.to_string(),
                    None => "unknown".into(),
                };
                self.enqueue(
                    dir,
                    &destination,
                    payload,
                    &format!("legacy:{:x}", hash.finalize()),
                    Some(&json!({"file":source,"line":index})),
                    now,
                )?;
                report.imported += 1;
                continue;
            }
            // Bind invalid UTF-8 as BLOB, preserving original bytes exactly.
            match std::str::from_utf8(&raw) {
                Ok(text) => {
                    self.con.execute("INSERT OR IGNORE INTO quarantine(source,line_number,raw,error) VALUES(?,?,?,'Invalid legacy JSON object')",params![source,index as i64,text])?;
                }
                Err(_) => {
                    self.con.execute("INSERT OR IGNORE INTO quarantine(source,line_number,raw,error) VALUES(?,?,?,'Invalid UTF-8')",params![source,index as i64,raw])?;
                }
            }
            report.quarantined += 1;
        }
        Ok(report)
    }
}

/// Bounded read-only inspection without creating tables or recovering leases.
pub fn read_delivery_page(
    con: &Connection,
    prefix: &str,
    after: i64,
    key: Option<&str>,
    limit: usize,
) -> Result<Vec<Delivery>> {
    if after < 0
        || !(1..=100).contains(&limit)
        || prefix.is_empty()
        || prefix.len() > 1024
        || prefix.chars().any(char::is_control)
    {
        return Err(Error::Invalid("Invalid delivery page"));
    }
    let table: bool = con.query_row("SELECT EXISTS(SELECT 1 FROM main.sqlite_schema WHERE name='delivery' AND type='table' AND rootpage>0)", [], |r|r.get(0))?;
    if !table {
        return Err(Error::Invalid("Missing delivery table"));
    }
    Ok(con.prepare("SELECT * FROM main.delivery WHERE direction='outbound' AND substr(dedup_key,1,length(?))=? AND id>? AND (? IS NULL OR dedup_key=?) AND length(payload)<=131072 AND length(origin)<=65536 AND (receipt IS NULL OR length(receipt)<=65536) ORDER BY id LIMIT ?")?
        .query_map(params![prefix,prefix,after,key,key,limit as i64], delivery)?.collect::<rusqlite::Result<_>>()?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    struct Temp(std::path::PathBuf);
    impl Temp {
        fn new() -> Self {
            Self(std::env::temp_dir().join(format!(
                "daedalus-journal-{}-{}",
                std::process::id(),
                COUNTER.fetch_add(1, Ordering::Relaxed)
            )))
        }
        fn db(&self) -> std::path::PathBuf {
            self.0.join("delivery.sqlite3")
        }
    }
    impl Drop for Temp {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    #[test]
    fn scheduler_pages_destinations_without_loading_payloads_or_history() {
        let mut journal = Journal::memory().unwrap();
        for (key, destination) in [
            ("a", "pane-a"),
            ("b", "pane-a"),
            ("c", "pane-b"),
            ("d", "pane-c"),
        ] {
            journal
                .enqueue(
                    "inbound",
                    destination,
                    &json!({"text":"fixture"}),
                    key,
                    None,
                    1.0,
                )
                .unwrap();
        }
        let first = journal
            .begin("inbound", "pane-a", "fixture", 1.0, 1.0)
            .unwrap()
            .unwrap();
        journal
            .finish(
                &first.delivery_id,
                first.lease_token.as_deref().unwrap(),
                "unknown",
                None,
                None,
                0.0,
                1.5,
            )
            .unwrap();
        // A corrupt historical body cannot break the scheduler; payloads are
        // decoded only when a destination's eligible delivery is claimed.
        journal
            .con
            .execute(
                "UPDATE delivery SET payload='invalid-json' WHERE state='unknown'",
                [],
            )
            .unwrap();
        assert_eq!(
            journal.destinations("inbound", "", 2).unwrap(),
            ["pane-a", "pane-b"]
        );
        assert_eq!(
            journal.destinations("inbound", "pane-b", 2).unwrap(),
            ["pane-c"]
        );
        assert!(journal.destinations("outbound", "", 2).unwrap().is_empty());
        assert!(journal.destinations("inbound", "", 0).is_err());
        assert!(journal.destinations("inbound", "", 1001).is_err());
    }
    fn enqueue(j: &mut Journal, key: &str) -> String {
        j.enqueue("outbound", "chat", &json!({"text":key}), key, None, 0.0)
            .unwrap()
    }

    #[test]
    fn admission_cursor_is_atomic_idempotent_and_monotonic() {
        let mut j = Journal::memory().unwrap();
        assert!(j.advance_cursor("telegram", "bot", 2).is_err());
        assert_eq!(j.cursor("telegram", "bot", 0).unwrap(), 0);
        assert!(
            j.admit_and_advance("telegram", "bot", 7, &json!({"text":"hi"}), 0.0)
                .unwrap()
        );
        assert!(
            !j.admit_and_advance("telegram", "bot", 7, &json!({"text":"hi"}), 0.0)
                .unwrap()
        );
        assert!(
            j.admit_and_advance("telegram", "bot", 7, &json!({"text":"changed"}), 0.0)
                .is_err()
        );
        j.admit_and_advance("telegram", "bot", 4, &json!({}), 0.0)
            .unwrap();
        assert_eq!(j.cursor("telegram", "bot", 0).unwrap(), 8);
        assert_eq!(j.cursor("telegram", "other-bot", 0).unwrap(), 0);
        assert_eq!(j.events("telegram", "bot", &["admitted"]).unwrap().len(), 2);
    }
    #[test]
    fn transaction_failure_cannot_leave_unacknowledged_partial_admission() {
        let mut j = Journal::memory().unwrap();
        j.con.execute_batch("CREATE TRIGGER reject_cursor BEFORE INSERT ON cursors BEGIN SELECT RAISE(ABORT,'test failure'); END;").unwrap();
        assert!(
            j.admit_and_advance("tg", "bot", 1, &json!({}), 0.0)
                .is_err()
        );
        assert!(j.events("tg", "bot", &["admitted"]).unwrap().is_empty());
    }
    #[test]
    fn imported_json_whitespace_does_not_break_deduplication() {
        let mut j = Journal::memory().unwrap();
        j.con.execute("INSERT INTO inbox(channel,bot,event_id,payload,state,created_at) VALUES('tg','bot','1','{ \"a\" : 2 }','admitted',0)",[]).unwrap();
        assert!(!j.admit("tg", "bot", "1", &json!({"a":2}), 0.0).unwrap());
        let id = enqueue(&mut j, "a");
        j.con.execute("UPDATE delivery SET payload=' { \"text\" : \"a\" } ',payload_sha='legacy-format' WHERE delivery_id=?",[&id]).unwrap();
        assert_eq!(enqueue(&mut j, "a"), id);
        assert!(
            j.enqueue(
                "outbound",
                "other-chat",
                &json!({"text":"a"}),
                "a",
                None,
                0.0
            )
            .is_err()
        );
        assert!(
            j.enqueue(
                "outbound",
                "chat",
                &json!({"text":"changed"}),
                "a",
                None,
                0.0
            )
            .is_err()
        );
    }
    #[test]
    fn lease_receipt_and_stale_owner_checks_preserve_payload() {
        let mut j = Journal::memory().unwrap();
        let id = enqueue(&mut j, "one");
        let row = j
            .begin("outbound", "chat", "worker", 0.0, 10.0)
            .unwrap()
            .unwrap();
        assert_eq!(row.attempts, 1);
        assert_eq!(row.lease_until, Some(10.0));
        let lease = row.lease_token.unwrap();
        assert!(
            j.finish(
                &id,
                "wrong",
                "delivered",
                Some(&json!({"message_id":4})),
                None,
                0.0,
                1.0
            )
            .is_err()
        );
        assert!(
            j.finish(&id, &lease, "delivered", None, None, 0.0, 1.0)
                .is_err()
        );
        j.checkpoint(&id, &lease, &json!({"transcript_row":2}), 0.5)
            .unwrap();
        j.finish(
            &id,
            &lease,
            "delivered",
            Some(&json!({"message_id":4})),
            None,
            0.0,
            1.0,
        )
        .unwrap();
        let row = j.rows().unwrap().remove(0);
        assert_eq!(row.state, "delivered");
        assert_eq!(row.receipt, Some(json!({"message_id":4})));
        assert_eq!(row.payload, json!({"text":"one"}));
        assert!(
            j.finish(&id, &lease, "not_sent", None, None, 0.0, 2.0)
                .is_err()
        );
    }
    #[test]
    fn expired_intent_never_replays_and_does_not_freeze_later_messages() {
        let mut j = Journal::memory().unwrap();
        let first = enqueue(&mut j, "first");
        let second = enqueue(&mut j, "second");
        let leased = j.begin("outbound", "chat", "a", 0.0, 1.0).unwrap().unwrap();
        assert_eq!(leased.delivery_id, first);
        let next = j.begin("outbound", "chat", "b", 2.0, 5.0).unwrap().unwrap();
        assert_eq!(next.delivery_id, second);
        assert_eq!(j.rows().unwrap()[0].state, "unknown");
        assert!(
            j.finish(
                &first,
                leased.lease_token.as_ref().unwrap(),
                "delivered",
                Some(&json!({"message_id":1})),
                None,
                0.0,
                2.0
            )
            .is_err()
        );
        assert!(
            j.begin("outbound", "chat", "c", 9.0, 5.0)
                .unwrap()
                .is_none()
        );
        j.reconcile(
            &first,
            "retryable",
            &json!({"verified":"transport never invoked"}),
        )
        .unwrap();
        assert_eq!(
            j.begin("outbound", "chat", "c", 10.0, 5.0)
                .unwrap()
                .unwrap()
                .delivery_id,
            first
        );
    }
    #[test]
    fn finish_after_expiry_records_uncertainty_even_without_next_begin() {
        let mut j = Journal::memory().unwrap();
        let id = enqueue(&mut j, "first");
        let row = j.begin("outbound", "chat", "a", 0.0, 1.0).unwrap().unwrap();
        assert!(
            j.finish(
                &id,
                row.lease_token.as_ref().unwrap(),
                "delivered",
                Some(&json!({"message_id":2})),
                None,
                0.0,
                1.0
            )
            .is_err()
        );
        assert_eq!(j.rows().unwrap()[0].state, "unknown");
        assert!(j.reconcile(&id, "delivered", &json!({})).is_err());
        j.reconcile(&id, "delivered", &json!({"verified_message_id":2}))
            .unwrap();
    }
    #[test]
    fn retry_delay_holds_eligible_queue_and_priority_is_explicit() {
        let mut j = Journal::memory().unwrap();
        let first = enqueue(&mut j, "first");
        let second = enqueue(&mut j, "second");
        let lease = j
            .begin("outbound", "chat", "a", 0.0, 10.0)
            .unwrap()
            .unwrap()
            .lease_token
            .unwrap();
        j.finish(&first, &lease, "not_sent", None, Some("offline"), 10.0, 1.0)
            .unwrap();
        assert!(
            j.begin("outbound", "chat", "a", 5.0, 10.0)
                .unwrap()
                .is_none()
        );
        j.set_priority(&second, -1).unwrap();
        let prioritized = j
            .begin("outbound", "chat", "a", 5.0, 10.0)
            .unwrap()
            .unwrap();
        assert_eq!(prioritized.delivery_id, second);
        assert!(
            j.begin("outbound", "chat", "b", 6.0, 10.0)
                .unwrap()
                .is_none()
        );
        j.finish(
            &second,
            prioritized.lease_token.as_deref().unwrap(),
            "delivered",
            Some(&json!({"message_id":42})),
            None,
            0.0,
            7.0,
        )
        .unwrap();
        assert_eq!(
            j.begin("outbound", "chat", "a", 11.0, 10.0)
                .unwrap()
                .unwrap()
                .delivery_id,
            first
        );
    }
    #[test]
    fn abandon_only_pending_and_recovery_only_processing_for_exact_bot() {
        let mut j = Journal::memory().unwrap();
        let id = enqueue(&mut j, "first");
        assert!(j.abandon(&id, "").is_err());
        j.abandon(&id, "operator cancelled").unwrap();
        assert!(j.abandon(&id, "again").is_err());
        j.admit("tg", "bot", "1", &json!({}), 0.0).unwrap();
        j.admit("tg", "other", "1", &json!({}), 0.0).unwrap();
        let a = j.events("tg", "bot", &["admitted"]).unwrap()[0].id;
        let b = j.events("tg", "other", &["admitted"]).unwrap()[0].id;
        j.event_state(a, "processing", None).unwrap();
        j.event_state(b, "processing", None).unwrap();
        assert_eq!(j.recover_events("tg", "bot").unwrap(), 1);
        assert_eq!(j.events("tg", "bot", &["unknown"]).unwrap().len(), 1);
        assert_eq!(j.events("tg", "other", &["processing"]).unwrap().len(), 1);
    }
    #[test]
    fn burst_dedup_sealing_and_finish_are_durable_and_unambiguous() {
        let mut j = Journal::memory().unwrap();
        let item = json!({"text":"hi"});
        let id = j.add_burst("chat", &item, "event1", 0.0).unwrap();
        assert_eq!(j.add_burst("chat", &item, "event1", 1.0).unwrap(), id);
        assert!(j.add_burst("other", &item, "event1", 1.0).is_err());
        j.add_burst("chat", &json!({"text":"there"}), "event2", 1.0)
            .unwrap();
        assert!(j.ready_bursts(1.2, 8.0, false, 2.0).unwrap().is_empty());
        let ready = j.ready_bursts(1.2, 8.0, false, 3.0).unwrap();
        assert_eq!(ready.len(), 1);
        assert_eq!(ready[0].items.len(), 2);
        assert!(j.finish_burst(id, "missing").is_err());
        let delivery = enqueue(&mut j, "burst");
        j.finish_burst(id, &delivery).unwrap();
        j.finish_burst(id, &delivery).unwrap();
        assert!(!j.has_bursts().unwrap());
        let other = enqueue(&mut j, "other");
        assert!(j.finish_burst(id, &other).is_err());
    }
    #[test]
    fn callback_tokens_are_opaque_short_and_bot_scoped() {
        let mut j = Journal::memory().unwrap();
        let token = j.callback_token("bot", "long action payload").unwrap();
        assert!(token.len() < 64);
        assert_eq!(
            j.callback_token("bot", "long action payload").unwrap(),
            token
        );
        assert_eq!(
            j.callback_data("bot", &token).unwrap(),
            Some("long action payload".into())
        );
        assert_eq!(j.callback_data("other", &token).unwrap(), None);
    }
    #[cfg(unix)]
    #[test]
    fn restart_preserves_events_intents_and_callbacks_in_private_database() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = Temp::new();
        let mut j = Journal::open(&tmp.db()).unwrap();
        let id = enqueue(&mut j, "pending");
        j.begin("outbound", "chat", "worker", 0.0, 1.0).unwrap();
        let token = j.callback_token("bot", "action").unwrap();
        drop(j);
        let mut reopened = Journal::open(&tmp.db()).unwrap();
        assert!(
            reopened
                .begin("outbound", "chat", "worker2", 2.0, 2.0)
                .unwrap()
                .is_none()
        );
        assert_eq!(reopened.rows().unwrap()[0].delivery_id, id);
        assert_eq!(reopened.rows().unwrap()[0].state, "unknown");
        assert_eq!(
            reopened.callback_data("bot", &token).unwrap(),
            Some("action".into())
        );
        assert_eq!(
            std::fs::metadata(tmp.db()).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
    #[cfg(unix)]
    #[test]
    fn symlink_and_shared_parent_are_rejected() {
        use std::os::unix::fs::{PermissionsExt, symlink};
        let tmp = Temp::new();
        std::fs::create_dir_all(&tmp.0).unwrap();
        std::fs::set_permissions(&tmp.0, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(Journal::open(&tmp.db()).is_err());
        std::fs::set_permissions(&tmp.0, std::fs::Permissions::from_mode(0o700)).unwrap();
        let target = tmp.0.join("target");
        std::fs::write(&target, b"unchanged").unwrap();
        symlink(&target, tmp.db()).unwrap();
        assert!(Journal::open(&tmp.db()).is_err());
        assert_eq!(std::fs::read(&target).unwrap(), b"unchanged");
    }
    #[test]
    fn legacy_import_is_streamed_repeatable_and_quarantines_bad_rows() {
        let tmp = Temp::new();
        std::fs::create_dir_all(&tmp.0).unwrap();
        let source = tmp.0.join("queue.jsonl");
        let raw = b"{\"win\":\"pane\",\"text\":\"hi\"}\r\nnot-json\n\xff\n[]\n";
        std::fs::write(&source, raw).unwrap();
        let mut j = Journal::memory().unwrap();
        let report = j.import_jsonl(&source, "outbound", "win", 0.0).unwrap();
        assert_eq!(
            report,
            ImportReport {
                imported: 1,
                quarantined: 3
            }
        );
        assert_eq!(
            j.import_jsonl(&source, "outbound", "win", 0.0).unwrap(),
            report
        );
        assert_eq!(j.rows().unwrap().len(), 1);
        assert_eq!(
            j.con
                .query_row("SELECT count(*) FROM quarantine", [], |r| r
                    .get::<_, i64>(0))
                .unwrap(),
            3
        );
        assert_eq!(std::fs::read(&source).unwrap(), raw);
    }
    #[test]
    fn invalid_timing_does_not_create_intent_or_retry() {
        let mut j = Journal::memory().unwrap();
        enqueue(&mut j, "item");
        for lease in [0.0, -1.0, f64::NAN, f64::INFINITY] {
            assert!(j.begin("outbound", "chat", "worker", 0.0, lease).is_err());
        }
        assert_eq!(j.rows().unwrap()[0].state, "pending");
    }

    #[test]
    fn expired_checkpoint_cannot_authorize_terminal_submission() {
        let mut j = Journal::memory().unwrap();
        let id = enqueue(&mut j, "one");
        let row = j
            .begin("outbound", "chat", "worker", 0.0, 1.0)
            .unwrap()
            .unwrap();
        assert!(
            j.checkpoint(
                &id,
                row.lease_token.as_deref().unwrap(),
                &json!({"boundary":1}),
                1.0
            )
            .is_err()
        );
        assert_eq!(j.rows().unwrap()[0].state, "unknown");
        assert_eq!(j.rows().unwrap()[0].receipt, None);
    }

    #[test]
    fn event_pagination_filters_before_decoding_historical_payloads() {
        let mut j = Journal::memory().unwrap();
        for id in 1..=3 {
            j.admit("tg", "bot", &id.to_string(), &json!({}), 0.0)
                .unwrap();
        }
        j.con
            .execute(
                "UPDATE inbox SET state='handled',payload='invalid historical data' WHERE id=1",
                [],
            )
            .unwrap();
        let first = j.events_page("tg", "bot", &["admitted"], 0, 1).unwrap();
        assert_eq!(first.len(), 1);
        assert_eq!(first[0].id, 2);
        let second = j
            .events_page("tg", "bot", &["admitted"], first[0].id, 1)
            .unwrap();
        assert_eq!(second.len(), 1);
        assert_eq!(second[0].id, 3);
        assert!(
            j.events_page("tg", "bot", &["admitted"], 3, 1)
                .unwrap()
                .is_empty()
        );
    }

    #[cfg(unix)]
    #[test]
    fn legacy_import_rejects_symlinks_and_fifo_without_waiting_for_writer() {
        use std::os::unix::{ffi::OsStrExt, fs::symlink};
        let tmp = Temp::new();
        std::fs::create_dir_all(&tmp.0).unwrap();
        let target = tmp.0.join("original");
        std::fs::write(&target, b"{}\n").unwrap();
        let link = tmp.0.join("link");
        symlink(&target, &link).unwrap();
        let mut j = Journal::memory().unwrap();
        assert!(j.import_jsonl(&link, "outbound", "win", 0.0).is_err());
        let fifo = tmp.0.join("fifo");
        let cpath = std::ffi::CString::new(fifo.as_os_str().as_bytes()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(cpath.as_ptr(), 0o600) }, 0);
        assert!(j.import_jsonl(&fifo, "outbound", "win", 0.0).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn unsafe_journal_paths_are_rejected_before_creating_directories() {
        use std::os::unix::fs::symlink;
        let tmp = Temp::new();
        assert!(Journal::open(&tmp.0.join("new/../db")).is_err());
        assert!(!tmp.0.exists());
        assert!(Journal::open(Path::new("relative-journal/db")).is_err());
        std::fs::create_dir_all(&tmp.0).unwrap();
        let outside = tmp.0.join("outside");
        std::fs::create_dir(&outside).unwrap();
        let link = tmp.0.join("link");
        symlink(&outside, &link).unwrap();
        assert!(Journal::open(&link.join("created/db")).is_err());
        assert!(!outside.join("created").exists());
    }
    #[test]
    fn latest_hook_prompt_uses_partial_scope_index_and_tolerates_legacy_invalid_json() {
        let mut journal = Journal::memory().unwrap();
        let scope = json!({"instance":"fixture","conversation":"turn","terminal":{"pane":"1"}});
        journal
            .admit(
                "hooks",
                "fixture",
                "prompt",
                &json!({"scope":scope,"event":"UserPromptSubmit"}),
                1.0,
            )
            .unwrap();
        journal
            .admit(
                "hooks",
                "fixture",
                "tool",
                &json!({"scope":scope,"event":"PostToolUse"}),
                2.0,
            )
            .unwrap();
        journal.con.execute("INSERT INTO inbox(channel,bot,event_id,payload,state,created_at) VALUES('hooks','fixture','legacy-malformed','not json','admitted',3)",[]).unwrap();
        assert_eq!(
            journal
                .latest_hook_prompt("fixture", &scope)
                .unwrap()
                .unwrap()
                .event_id,
            "prompt"
        );
        journal
            .con
            .execute_batch("DROP INDEX hook_prompt_scope;")
            .unwrap();
        journal.con.execute_batch(SCHEMA).unwrap();
        let details:String=journal.con.query_row("EXPLAIN QUERY PLAN SELECT * FROM inbox WHERE channel='hooks' AND bot=? AND CASE WHEN json_valid(payload) THEN json_extract(payload,'$.event')='UserPromptSubmit' ELSE 0 END AND json_extract(payload,'$.scope')=json(?) ORDER BY id DESC LIMIT 1",params!["fixture",scope.to_string()],|r|r.get(3)).unwrap();
        assert!(details.contains("hook_prompt_scope"), "{details}");
        assert_eq!(journal.hook_head("fixture").unwrap(), 3);
    }
}
