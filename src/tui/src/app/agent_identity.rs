//! Armonia identity renames. Numeric identities and reserved aliases never change.
//! A durable intent precedes terminal metadata changes. Status observes; it never retries.
use crate::{
    adapters::terminals::{self, Terminals},
    core::{
        identity::{Backend, TerminalId},
        layouts,
        routes::Scope,
    },
};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, OpenOptions},
    io::{Read, Write},
    os::{
        fd::{AsRawFd, FromRawFd, OwnedFd},
        unix::{
            ffi::OsStrExt,
            fs::{FileTypeExt, MetadataExt, OpenOptionsExt},
            net::UnixStream,
        },
    },
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
    time::{Duration, Instant},
};
type Result<T> = std::result::Result<T, String>;
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
struct Binding {
    id: i64,
    base: String,
    display: String,
    backend: String,
    server: String,
    terminal_id: String,
    pane_id: String,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Observation {
    pub terminal: TerminalId,
    pub label: String,
    pub group: String,
    pub group_label: String,
    pub members: BTreeSet<(String, String)>,
    pub tokens: BTreeMap<String, String>,
    #[serde(default)]
    pub metadata_socket: Option<PathBuf>,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Plan {
    pub scope: Scope,
    pub actor: String,
    pub operation: String,
    pub display: String,
    pub base: String,
    binding: Binding,
    pane: Value,
    before: Observation,
    event: String,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Receipt {
    pub operation: String,
    pub agent_id: i64,
    pub display: String,
    pub state: String,
}
pub trait BackendApi {
    fn observe(&mut self, pane: &Value) -> Result<Observation>;
    fn rename(&mut self, plan: &Plan) -> Result<()>;
}
/// Exact socket paths are supplied by configuration, not inferred from labels or HOME.
pub struct Native<'a> {
    pub terminals: &'a Terminals,
    pub herdr_sockets: BTreeMap<String, PathBuf>,
}
static REQUEST: AtomicU64 = AtomicU64::new(0);
fn connect_socket(path: &Path) -> Result<UnixStream> {
    let bytes = path.as_os_str().as_bytes();
    let mut address: libc::sockaddr_un = unsafe { std::mem::zeroed() };
    if bytes.len() >= address.sun_path.len() || bytes.contains(&0) {
        return Err("Invalid Herdr socket path".into());
    }
    address.sun_family = libc::AF_UNIX as libc::sa_family_t;
    for (to, from) in address.sun_path.iter_mut().zip(bytes) {
        *to = *from as libc::c_char;
    }
    let raw = unsafe {
        libc::socket(
            libc::AF_UNIX,
            libc::SOCK_STREAM | libc::SOCK_CLOEXEC | libc::SOCK_NONBLOCK,
            0,
        )
    };
    if raw < 0 {
        return Err("Cannot open Herdr socket".into());
    }
    let fd = unsafe { OwnedFd::from_raw_fd(raw) };
    let length =
        (std::mem::offset_of!(libc::sockaddr_un, sun_path) + bytes.len() + 1) as libc::socklen_t;
    if unsafe { libc::connect(raw, (&address as *const libc::sockaddr_un).cast(), length) } < 0 {
        let errno = std::io::Error::last_os_error().raw_os_error();
        if !matches!(errno, Some(libc::EINPROGRESS) | Some(libc::EAGAIN)) {
            return Err("Herdr socket connection failed".into());
        }
        let mut event = libc::pollfd {
            fd: raw,
            events: libc::POLLOUT,
            revents: 0,
        };
        if unsafe { libc::poll(&mut event, 1, 2000) } <= 0 {
            return Err("Herdr socket connection timed out".into());
        }
        let mut error = 0i32;
        let mut size = std::mem::size_of::<i32>() as libc::socklen_t;
        if unsafe {
            libc::getsockopt(
                raw,
                libc::SOL_SOCKET,
                libc::SO_ERROR,
                (&mut error as *mut i32).cast(),
                &mut size,
            )
        } != 0
            || error != 0
        {
            return Err("Herdr socket connection unconfirmed".into());
        }
    }
    let mut peer: libc::ucred = unsafe { std::mem::zeroed() };
    let mut size = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
    if unsafe {
        libc::getsockopt(
            raw,
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            (&mut peer as *mut libc::ucred).cast(),
            &mut size,
        )
    } != 0
        || peer.uid != unsafe { libc::geteuid() }
    {
        return Err("Herdr socket peer ownership unverified".into());
    }
    let stream = UnixStream::from(fd);
    stream
        .set_nonblocking(false)
        .map_err(|_| "Cannot configure metadata socket")?;
    Ok(stream)
}
impl Native<'_> {
    fn rpc(&self, server: &str, method: &str, params: Value) -> Result<Value> {
        let path = self
            .herdr_sockets
            .get(server)
            .ok_or("Herdr metadata socket is not configured")?;
        layouts::check_path(path)?;
        let metadata = fs::symlink_metadata(path).map_err(|_| "Herdr socket unavailable")?;
        if !metadata.file_type().is_socket() || metadata.uid() != unsafe { libc::geteuid() } {
            return Err("Herdr metadata socket has unverified ownership".into());
        }
        let mut stream = connect_socket(path)?;
        stream
            .set_read_timeout(Some(Duration::from_secs(2)))
            .map_err(|_| "Cannot bound metadata read")?;
        stream
            .set_write_timeout(Some(Duration::from_secs(2)))
            .map_err(|_| "Cannot bound metadata write")?;
        let request = format!(
            "daedalus-identity-{}-{}",
            std::process::id(),
            REQUEST.fetch_add(1, Ordering::Relaxed)
        );
        let mut bytes = serde_json::to_vec(&json!({"id":request,"method":method,"params":params}))
            .map_err(|_| "Cannot encode metadata request")?;
        bytes.push(b'\n');
        stream
            .write_all(&bytes)
            .map_err(|_| "Herdr metadata submission unconfirmed")?;
        let start = Instant::now();
        let mut response = Vec::new();
        let mut chunk = [0; 4096];
        loop {
            if start.elapsed() > Duration::from_secs(5) || response.len() > 1048576 {
                return Err("Herdr metadata receipt exceeded limits".into());
            }
            let read = stream
                .read(&mut chunk)
                .map_err(|_| "Herdr metadata receipt unavailable")?;
            if read == 0 {
                return Err("Herdr metadata receipt incomplete".into());
            }
            if let Some(end) = chunk[..read].iter().position(|b| *b == b'\n') {
                response.extend_from_slice(&chunk[..end]);
                break;
            }
            response.extend_from_slice(&chunk[..read]);
        }
        let value: Value =
            serde_json::from_slice(&response).map_err(|_| "Invalid Herdr metadata receipt")?;
        if value["id"] != request || value.get("error").is_some() || !value["result"].is_object() {
            return Err("Herdr metadata receipt unconfirmed".into());
        }
        Ok(value["result"].clone())
    }
}
impl BackendApi for Native<'_> {
    fn observe(&mut self, pane: &Value) -> Result<Observation> {
        let actual = self.terminals.verify(pane)?;
        let terminal = terminals::identity(&actual)?;
        if terminal.backend == Backend::Herdr {
            let result = self.rpc(
                &terminal.endpoint,
                "pane.get",
                json!({"pane_id":terminal.pane}),
            )?;
            let row = &result["pane"];
            if row["terminal_id"] != terminal.generation || row["pane_id"] != terminal.pane {
                return Err("Herdr metadata terminal identity changed".into());
            }
            let tokens = row["tokens"]
                .as_object()
                .map(|tokens| {
                    tokens
                        .iter()
                        .filter_map(|(k, v)| v.as_str().map(|v| (k.clone(), v.to_owned())))
                        .collect()
                })
                .unwrap_or_default();
            let metadata_socket = self.herdr_sockets.get(&terminal.endpoint).cloned();
            return Ok(Observation {
                metadata_socket,
                terminal,
                label: String::new(),
                group: String::new(),
                group_label: String::new(),
                members: BTreeSet::new(),
                tokens,
            });
        }
        let inventory = self.terminals.tmux_inventory(&terminal.endpoint)?;
        let group = actual["window_id"]
            .as_str()
            .ok_or("Missing tmux window identity")?
            .to_owned();
        let mut members = BTreeSet::new();
        for row in inventory["panes"]
            .as_array()
            .ok_or("Missing tmux membership")?
        {
            if row["window_id"] == group {
                let id = terminals::identity(row)?;
                if id.endpoint != terminal.endpoint {
                    return Err("Mixed tmux identity inventory".into());
                }
                members.insert((id.pane, id.generation));
            }
        }
        if !members.contains(&(terminal.pane.clone(), terminal.generation.clone())) {
            return Err("Pane left its tmux window".into());
        }
        Ok(Observation {
            terminal,
            label: actual["title"].as_str().unwrap_or("").into(),
            group,
            group_label: actual["window"].as_str().unwrap_or("").into(),
            members,
            tokens: BTreeMap::new(),
            metadata_socket: None,
        })
    }
    fn rename(&mut self, plan: &Plan) -> Result<()> {
        if self.observe(&plan.pane)? != plan.before {
            return Err("Terminal metadata changed before mutation".into());
        }
        let id = &plan.before.terminal;
        if id.backend == Backend::Herdr {
            self.rpc(&id.endpoint,"pane.report_metadata",json!({"pane_id":id.pane,"source":"armonia","tokens":{"armonia_id":plan.binding.id.to_string(),"name":plan.display,"use":plan.display}}))?;
            return Ok(());
        }
        self.terminals.query(
            &self.terminals.tmux,
            &[
                "-S",
                &id.endpoint,
                "select-pane",
                "-t",
                &id.pane,
                "-T",
                &plan.display,
            ],
        )?;
        if plan.before.members.len() == 1 {
            let current = self.observe(&plan.pane)?;
            if !same_identity(&plan.before, &current) {
                return Err("Window membership changed after pane rename".into());
            }
            self.terminals.query(
                &self.terminals.tmux,
                &[
                    "-S",
                    &id.endpoint,
                    "rename-window",
                    "-t",
                    &plan.before.group,
                    &plan.display,
                ],
            )?;
        }
        Ok(())
    }
}
fn same_identity(a: &Observation, b: &Observation) -> bool {
    a.terminal == b.terminal
        && a.group == b.group
        && a.members == b.members
        && a.metadata_socket == b.metadata_socket
}
fn confirmed(plan: &Plan, actual: &Observation) -> bool {
    same_identity(&plan.before, actual)
        && if actual.terminal.backend == Backend::Herdr {
            actual.tokens.get("armonia_id") == Some(&plan.binding.id.to_string())
                && actual.tokens.get("name") == Some(&plan.display)
                && actual.tokens.get("use") == Some(&plan.display)
        } else {
            actual.label == plan.display
                && (actual.members.len() != 1 || actual.group_label == plan.display)
        }
}
fn display(base: &str, id: i64) -> Result<(String, String)> {
    if id <= 0
        || base.is_empty()
        || base.len() > 256
        || base.chars().any(|c| c.is_whitespace() || c.is_control())
    {
        return Err("Use one printable agent name token".into());
    }
    let suffix = format!("-{id}");
    let stem = base.strip_suffix(&suffix).unwrap_or(base);
    let mut slug = String::new();
    for c in stem.to_lowercase().chars() {
        let c = if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
            c
        } else {
            '-'
        };
        if c != '-' || !slug.ends_with('-') {
            slug.push(c);
        }
    }
    let slug = slug.trim_matches(['-', '_']);
    let slug = if slug.is_empty() { "agent" } else { slug };
    let max = 63 - suffix.len();
    Ok((
        stem.into(),
        format!(
            "{}{}",
            slug[..slug.len().min(max)].trim_end_matches(['-', '_']),
            suffix
        ),
    ))
}
fn binding(con: &Connection, target: &str) -> Result<Binding> {
    let mut query=con.prepare("SELECT DISTINCT a.id,a.base,a.display,b.backend,b.server,b.terminal_id,b.pane_id FROM agents a JOIN bindings b ON b.agent_id=a.id LEFT JOIN aliases x ON x.agent_id=a.id WHERE (a.display=?1 OR x.token=?1) AND b.availability='live' LIMIT 2").map_err(|_|"Armonia identity schema unavailable")?;
    let rows = query
        .query_map([target], |r| {
            Ok(Binding {
                id: r.get(0)?,
                base: r.get(1)?,
                display: r.get(2)?,
                backend: r.get(3)?,
                server: r.get(4)?,
                terminal_id: r.get(5)?,
                pane_id: r.get(6)?,
            })
        })
        .map_err(|_| "Cannot resolve exact agent")?
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(|_| "Invalid agent binding")?;
    if rows.len() != 1 {
        return Err("Use one exact live Armonia agent identity".into());
    }
    Ok(rows[0].clone())
}
fn collision(con: &Connection, name: &str, id: i64) -> Result<()> {
    let conflict:bool=con.query_row("SELECT EXISTS(SELECT 1 FROM aliases WHERE token=?1 AND agent_id<>?2 UNION ALL SELECT 1 FROM agents WHERE display=?1 AND id<>?2)",params![name,id],|r|r.get(0)).map_err(|_|"Cannot verify alias ownership")?;
    if conflict {
        return Err("Agent name is reserved by another numeric identity".into());
    }
    Ok(())
}
fn registry(directory: &Path) -> Result<PathBuf> {
    let path = directory.join("armonia.sqlite3");
    layouts::check_path(&path)?;
    if !path.is_file() {
        return Err("Optional Armonia registry is not configured".into());
    }
    Ok(path)
}
fn validate_registry(con: &Connection) -> Result<()> {
    let version: i64 = con
        .query_row("PRAGMA user_version", [], |r| r.get(0))
        .map_err(|_| "Cannot inspect Armonia schema")?;
    let triggers: i64 = con
        .query_row(
            "SELECT count(*) FROM sqlite_schema WHERE type='trigger'",
            [],
            |r| r.get(0),
        )
        .map_err(|_| "Cannot inspect Armonia schema")?;
    if version != 1 || triggers != 0 {
        return Err("Unsupported Armonia registry schema".into());
    }
    Ok(())
}
/// Scope/actor are provenance, not authorization. The frontend must verify its
/// current grants before preparing, applying, or reconciling this operation.
pub struct Request<'a> {
    pub scope: Scope,
    pub actor: &'a str,
    pub target: &'a str,
    pub base: &'a str,
    pub event: &'a str,
    pub pane: Value,
}
impl Plan {
    pub fn prepare(
        directory: &Path,
        request: Request<'_>,
        backend: &mut impl BackendApi,
    ) -> Result<Self> {
        let Request {
            scope,
            actor,
            target,
            base,
            event,
            pane,
        } = request;
        scope.validate().map_err(|_| "Invalid rename scope")?;
        if actor.is_empty() || actor.len() > 128 || event.is_empty() || event.len() > 512 {
            return Err("Rename requires a stable authorized actor and event".into());
        }
        let path = registry(directory)?;
        let binding = super::diagnostics::with_database(&path, |con| {
            validate_registry(con)?;
            binding(con, target)
        })?
        .ok_or("Armonia registry unavailable")?;
        let before = backend.observe(&pane)?;
        let backend_name = if before.terminal.backend == Backend::Tmux {
            "tmux"
        } else {
            "herdr"
        };
        if binding.backend != backend_name
            || binding.pane_id != before.terminal.pane
            || binding.terminal_id != before.terminal.generation
            || pane["server"] != binding.server
        {
            return Err("Armonia binding no longer owns selected terminal".into());
        }
        let (base, display) = display(base, binding.id)?;
        super::diagnostics::with_database(&path, |con| collision(con, &display, binding.id))?;
        let event = format!(
            "native:{}",
            serde_json::to_string(&(scope.clone(), actor, event))
                .map_err(|_| "Cannot encode rename event")?
        );
        let operation = format!("ar:{:x}", Sha256::digest(event.as_bytes()));
        Ok(Self {
            scope,
            actor: actor.into(),
            operation,
            display,
            base,
            binding,
            pane,
            before,
            event,
        })
    }
}
struct Lock(std::fs::File);
impl Drop for Lock {
    fn drop(&mut self) {
        unsafe {
            libc::flock(self.0.as_raw_fd(), libc::LOCK_UN);
        }
    }
}
fn lock(directory: &Path) -> Result<Lock> {
    let path = directory.join("armonia.sqlite3.organization.lock");
    layouts::check_path(&path)?;
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
        .map_err(|_| "Cannot lock Armonia organization")?;
    let metadata = file
        .metadata()
        .map_err(|_| "Cannot inspect organization lock")?;
    if !metadata.is_file()
        || metadata.uid() != unsafe { libc::geteuid() }
        || metadata.nlink() != 1
        || metadata.mode() & 0o077 != 0
    {
        return Err("Unsafe Armonia organization lock".into());
    }
    if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
        return Err("Armonia organization is busy".into());
    }
    Ok(Lock(file))
}
fn writer(directory: &Path) -> Result<Connection> {
    let path = registry(directory)?;
    let con = super::telegram_config::private_connection(&path)?;
    validate_registry(&con)?;
    con.execute_batch("CREATE TABLE IF NOT EXISTS naming_operations(op TEXT PRIMARY KEY,event_key TEXT UNIQUE,agent_id INTEGER,name TEXT,base TEXT,state TEXT,receipt TEXT,error TEXT,created_at REAL);PRAGMA synchronous=FULL;").map_err(|_|"Cannot initialize naming journal")?;
    Ok(con)
}
fn receipt(plan: &Plan, state: &str) -> Receipt {
    Receipt {
        operation: plan.operation.clone(),
        agent_id: plan.binding.id,
        display: plan.display.clone(),
        state: state.into(),
    }
}
pub fn apply(directory: &Path, plan: Plan, backend: &mut impl BackendApi) -> Result<Receipt> {
    let _guard = super::hook_runtime::active_guard(directory).map_err(|e| e.to_string())?;
    let _lock = lock(directory)?;
    let mut con = writer(directory)?;
    let tx = con
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .map_err(|_| "Cannot begin rename intent")?;
    if let Some((prior, state)) = tx
        .query_row(
            "SELECT receipt,state FROM naming_operations WHERE event_key=?",
            [&plan.event],
            |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)),
        )
        .optional()
        .map_err(|_| "Cannot inspect prior rename")?
    {
        let old: Plan = serde_json::from_str(&prior)
            .map_err(|_| "Prior rename requires legacy reconciliation")?;
        if old.display != plan.display
            || old.binding != plan.binding
            || old.scope != plan.scope
            || old.actor != plan.actor
        {
            return Err("Rename event reused for another operation".into());
        }
        return Ok(receipt(&old, &state));
    }
    if binding(&tx, &plan.binding.display)? != plan.binding {
        return Err("Registry binding changed before rename".into());
    }
    let pending:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM naming_operations WHERE agent_id=? AND state IN ('intent','unknown'))",[plan.binding.id],|r|r.get(0)).map_err(|_|"Cannot inspect uncertain renames")?;
    if pending {
        return Err("Prior agent rename requires reconciliation".into());
    }
    collision(&tx, &plan.display, plan.binding.id)?;
    if backend.observe(&plan.pane)? != plan.before {
        return Err("Terminal identity or metadata changed; review again".into());
    }
    tx.execute(
        "INSERT OR IGNORE INTO aliases(token,agent_id) VALUES(?,?)",
        params![plan.display, plan.binding.id],
    )
    .map_err(|_| "Cannot reserve agent alias")?;
    tx.execute(
        "INSERT INTO naming_operations VALUES(?,?,?,?,?,'intent',?,NULL,?)",
        params![
            plan.operation,
            plan.event,
            plan.binding.id,
            plan.display,
            plan.base,
            serde_json::to_string(&plan).map_err(|_| "Cannot encode rename intent")?,
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_err(|_| "Invalid system clock")?
                .as_secs_f64()
        ],
    )
    .map_err(|_| "Cannot persist rename intent")?;
    tx.commit().map_err(|_| "Cannot commit rename intent")?;
    let _attempt = backend.rename(&plan);
    settle(&mut con, &plan, backend)
}
fn settle(con: &mut Connection, plan: &Plan, backend: &mut impl BackendApi) -> Result<Receipt> {
    let verified = backend
        .observe(&plan.pane)
        .is_ok_and(|actual| confirmed(plan, &actual));
    let tx = con
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .map_err(|_| "Rename outcome not recorded; inspect operation status")?;
    let registry_matches =
        binding(&tx, &plan.binding.display).is_ok_and(|actual| actual == plan.binding);
    let state = if verified && registry_matches {
        "verified"
    } else {
        "unknown"
    };
    if state == "verified" {
        collision(&tx, &plan.display, plan.binding.id)?;
        tx.execute(
            "INSERT OR IGNORE INTO aliases(token,agent_id) VALUES(?,?)",
            params![plan.binding.display, plan.binding.id],
        )
        .map_err(|_| "Cannot preserve previous alias")?;
        tx.execute(
            "UPDATE agents SET base=?,display=? WHERE id=?",
            params![plan.base, plan.display, plan.binding.id],
        )
        .map_err(|_| "Cannot commit canonical agent name")?;
    }
    tx.execute(
        "UPDATE naming_operations SET state=?,error=? WHERE op=?",
        params![
            state,
            if state == "unknown" {
                Some("Outcome requires exact metadata reconciliation")
            } else {
                None
            },
            plan.operation
        ],
    )
    .map_err(|_| "Cannot record rename outcome")?;
    tx.commit()
        .map_err(|_| "Rename outcome remains unconfirmed")?;
    Ok(receipt(plan, state))
}
/// Reconciliation only reads backend metadata; no terminal mutation is retried.
pub fn status(
    directory: &Path,
    scope: &Scope,
    actor: &str,
    operation: &str,
    backend: &mut impl BackendApi,
) -> Result<Receipt> {
    let _guard = super::hook_runtime::active_guard(directory).map_err(|e| e.to_string())?;
    let _lock = lock(directory)?;
    let mut con = writer(directory)?;
    let (encoded, state): (String, String) = con
        .query_row(
            "SELECT receipt,state FROM naming_operations WHERE op=?",
            [operation],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .map_err(|_| "Agent rename operation unavailable")?;
    let plan: Plan = serde_json::from_str(&encoded)
        .map_err(|_| "Legacy rename requires its original reconciler")?;
    if &plan.scope != scope || plan.actor != actor {
        return Err("Agent rename belongs to another caller scope".into());
    }
    if state == "verified" {
        return Ok(receipt(&plan, &state));
    }
    settle(&mut con, &plan, backend)
}

pub const HELP: &str = "daedalus agent-identity preview|apply --config-dir ABS --target ABS --agent EXACT_NAME --name NEW_BASE --event ID [--herdr-socket ABS] [--approve DIGEST]\ndaedalus agent-identity status --config-dir ABS --operation ar:ID [--herdr-socket ABS]\nAn existing optional Armonia registry and exact terminal observation are required. Preview never renames. Apply requires its unchanged digest. Status reconciles observations without retrying.\n";
impl Plan {
    pub fn approval(&self) -> Result<String> {
        Ok(format!(
            "{:x}",
            Sha256::digest(serde_json::to_vec(self).map_err(|_| "Cannot encode identity review")?)
        ))
    }
    pub fn review(&self) -> Result<Value> {
        Ok(
            json!({"approval":self.approval()?,"operation":self.operation,"agent_id":self.binding.id,"previous":self.binding.display,"display":self.display,"terminal":self.before.terminal,"aliases":"Previous aliases remain reserved","uncertainty":"An attempted mutation is never automatically replayed"}),
        )
    }
}
struct Cli {
    verb: String,
    directory: PathBuf,
    options: BTreeMap<String, String>,
}
fn cli(args: &[String]) -> Result<Option<Cli>> {
    if args.is_empty() || args == ["--help"] || args == ["-h"] {
        return Ok(None);
    }
    if !matches!(args[0].as_str(), "preview" | "apply" | "status")
        || !(args.len() - 1).is_multiple_of(2)
    {
        return Err("Invalid agent-identity command; see --help".into());
    }
    let mut options = BTreeMap::new();
    for pair in args[1..].chunks_exact(2) {
        if !matches!(
            pair[0].as_str(),
            "--config-dir"
                | "--target"
                | "--agent"
                | "--name"
                | "--event"
                | "--herdr-socket"
                | "--operation"
                | "--approve"
        ) || options.insert(pair[0].clone(), pair[1].clone()).is_some()
        {
            return Err("Unknown or duplicate identity option".into());
        }
    }
    let directory = PathBuf::from(
        options
            .remove("--config-dir")
            .ok_or("Missing --config-dir")?,
    );
    layouts::check_path(&directory)?;
    let required = if args[0] == "status" {
        vec!["--operation"]
    } else {
        vec!["--target", "--agent", "--name", "--event"]
    };
    if required.iter().any(|key| !options.contains_key(*key))
        || options.keys().any(|key| {
            !required.contains(&key.as_str())
                && key != "--herdr-socket"
                && !(key == "--approve" && args[0] == "apply")
        })
        || (args[0] == "apply" && !options.contains_key("--approve"))
    {
        return Err("Missing or incompatible identity options".into());
    }
    Ok(Some(Cli {
        verb: args[0].clone(),
        directory,
        options,
    }))
}
fn local_scope(directory: &Path) -> Result<(Scope, String)> {
    let bytes = super::telegram_config::read_private(&directory.join("control.json"), 1048576)?
        .ok_or("Instance configuration missing")?;
    let config = super::config::ControlConfig::parse(&bytes)?;
    let actor = format!("uid:{}", unsafe { libc::geteuid() });
    Ok((
        Scope {
            instance: config.agent_name,
            bot: "local-cli".into(),
            chat: actor.clone(),
        },
        actor,
    ))
}
fn execute_cli(command: &Cli, backend: &mut impl BackendApi) -> Result<Value> {
    let (scope, actor) = local_scope(&command.directory)?;
    if command.verb == "status" {
        return serde_json::to_value(status(
            &command.directory,
            &scope,
            &actor,
            &command.options["--operation"],
            backend,
        )?)
        .map_err(|_| "Cannot encode identity receipt".into());
    }
    let path = Path::new(&command.options["--target"]);
    let bytes = super::telegram_config::read_private(path, 1048576)?
        .ok_or("Exact target observation is missing")?;
    let pane = serde_json::from_slice(&bytes).map_err(|_| "Invalid target observation")?;
    let plan = Plan::prepare(
        &command.directory,
        Request {
            scope,
            actor: &actor,
            target: &command.options["--agent"],
            base: &command.options["--name"],
            event: &command.options["--event"],
            pane,
        },
        backend,
    )?;
    if command.verb == "preview" {
        return plan.review();
    }
    if plan.approval()? != command.options["--approve"] {
        return Err("Identity review changed; preview again".into());
    }
    serde_json::to_value(apply(&command.directory, plan, backend)?)
        .map_err(|_| "Cannot encode identity receipt".into())
}
pub fn run(args: &[String], out: &mut impl Write) -> Result<()> {
    let Some(command) = cli(args)? else {
        return out
            .write_all(HELP.as_bytes())
            .map_err(|_| "Cannot write identity help".into());
    };
    let mut sockets = BTreeMap::new();
    if let Some(socket) = command.options.get("--herdr-socket") {
        let path = PathBuf::from(socket);
        layouts::check_path(&path)?;
        let terminal = if command.verb == "status" {
            let encoded =
                super::diagnostics::with_database(&registry(&command.directory)?, |con| {
                    con.query_row(
                        "SELECT receipt FROM naming_operations WHERE op=?",
                        [&command.options["--operation"]],
                        |row| row.get::<_, String>(0),
                    )
                    .map_err(|_| "Identity operation unavailable".into())
                })?
                .ok_or("Identity journal unavailable")?;
            serde_json::from_str::<Plan>(&encoded)
                .map_err(|_| "Invalid identity operation")?
                .before
                .terminal
        } else {
            let bytes = super::telegram_config::read_private(
                Path::new(&command.options["--target"]),
                1048576,
            )?
            .ok_or("Target missing")?;
            terminals::identity(&serde_json::from_slice(&bytes).map_err(|_| "Invalid target")?)?
        };
        if terminal.backend != Backend::Herdr {
            return Err("Herdr socket is only valid for a Herdr target".into());
        }
        sockets.insert(terminal.endpoint, path);
    }
    let terminals = Terminals::default();
    let mut backend = Native {
        terminals: &terminals,
        herdr_sockets: sockets,
    };
    let result = execute_cli(&command, &mut backend)?;
    serde_json::to_writer_pretty(&mut *out, &result).map_err(|_| "Cannot write identity result")?;
    out.write_all(b"\n")
        .map_err(|_| "Cannot write identity result".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    struct Fixture(PathBuf);
    impl Fixture {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "daedalus-agent-identity-{}-{}",
                std::process::id(),
                REQUEST.fetch_add(1, Ordering::Relaxed)
            ));
            layouts::write_private(&path.join("armonia.sqlite3"), b"").unwrap();
            let con =
                super::super::telegram_config::private_connection(&path.join("armonia.sqlite3"))
                    .unwrap();
            con.execute_batch("PRAGMA user_version=1; CREATE TABLE agents(id INTEGER PRIMARY KEY,base TEXT,display TEXT UNIQUE);CREATE TABLE aliases(token TEXT PRIMARY KEY,agent_id INTEGER);CREATE TABLE bindings(agent_id INTEGER PRIMARY KEY,backend TEXT,server TEXT,terminal_id TEXT,pane_id TEXT,availability TEXT);INSERT INTO agents VALUES(7,'old','old-7');INSERT INTO aliases VALUES('old-7',7);INSERT INTO bindings VALUES(7,'tmux','tmux@fixture','generation','%1','live');").unwrap();
            Self(path)
        }
        fn con(&self) -> Connection {
            Connection::open(self.0.join("armonia.sqlite3")).unwrap()
        }
        fn plan(&self, backend: &mut Mock, event: &str) -> Plan {
            Plan::prepare(
                &self.0,
                Request {
                    scope: scope(),
                    actor: "1",
                    target: "old-7",
                    base: "new",
                    event,
                    pane: pane(),
                },
                backend,
            )
            .unwrap()
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
    fn scope() -> Scope {
        Scope {
            instance: "example".into(),
            bot: "123".into(),
            chat: "42".into(),
        }
    }
    fn pane() -> Value {
        json!({"backend":"tmux","socket":"/fixture/socket","server":"tmux@fixture","pane_id":"%1","terminal_id":"generation"})
    }
    struct Mock {
        observation: Observation,
        calls: usize,
        complete: bool,
    }
    impl Mock {
        fn new() -> Self {
            Self {
                observation: Observation {
                    terminal: terminals::identity(&pane()).unwrap(),
                    label: "old-7".into(),
                    group: "@1".into(),
                    group_label: "old-7".into(),
                    members: BTreeSet::from([("%1".into(), "generation".into())]),
                    tokens: BTreeMap::new(),
                    metadata_socket: None,
                },
                calls: 0,
                complete: true,
            }
        }
    }
    impl BackendApi for Mock {
        fn observe(&mut self, _: &Value) -> Result<Observation> {
            Ok(self.observation.clone())
        }
        fn rename(&mut self, plan: &Plan) -> Result<()> {
            self.calls += 1;
            if self.complete {
                self.observation.label = plan.display.clone();
                self.observation.group_label = plan.display.clone();
            }
            Err("Simulated lost acknowledgement".into())
        }
    }
    #[test]
    fn verified_receipt_preserves_numeric_identity_and_aliases_despite_lost_ack() {
        let f = Fixture::new();
        let mut backend = Mock::new();
        let before = fs::read(f.0.join("armonia.sqlite3")).unwrap();
        let plan = f.plan(&mut backend, "event");
        assert_eq!(fs::read(f.0.join("armonia.sqlite3")).unwrap(), before);
        let result = apply(&f.0, plan.clone(), &mut backend).unwrap();
        assert_eq!(result.state, "verified");
        assert_eq!(result.agent_id, 7);
        assert_eq!(
            f.con()
                .query_row("SELECT id,display FROM agents", [], |r| Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, String>(1)?
                )))
                .unwrap(),
            (7, "new-7".into())
        );
        assert_eq!(
            f.con()
                .query_row("SELECT count(*) FROM aliases WHERE agent_id=7", [], |r| r
                    .get::<_, i64>(
                    0
                ))
                .unwrap(),
            2
        );
        assert_eq!(
            apply(&f.0, plan.clone(), &mut backend).unwrap().state,
            "verified"
        );
        assert_eq!(backend.calls, 1);
        assert_eq!(
            status(&f.0, &scope(), "1", &plan.operation, &mut backend)
                .unwrap()
                .state,
            "verified"
        );
        assert_eq!(backend.calls, 1);
    }
    #[test]
    fn changed_metadata_socket_invalidates_review_without_mutation() {
        let f = Fixture::new();
        let mut backend = Mock::new();
        backend.observation.metadata_socket = Some("/tmp/fixture/old.sock".into());
        let plan = f.plan(&mut backend, "socket-event");
        backend.observation.metadata_socket = Some("/tmp/fixture/new.sock".into());
        assert!(apply(&f.0, plan, &mut backend).is_err());
        assert_eq!(backend.calls, 0);
    }
    #[test]
    fn uncertain_attempt_is_not_replayed_and_status_only_settles_observed_labels() {
        let f = Fixture::new();
        let mut backend = Mock::new();
        backend.complete = false;
        let plan = f.plan(&mut backend, "event");
        assert_eq!(
            apply(&f.0, plan.clone(), &mut backend).unwrap().state,
            "unknown"
        );
        assert_eq!(
            apply(&f.0, plan.clone(), &mut backend).unwrap().state,
            "unknown"
        );
        assert_eq!(backend.calls, 1);
        let next = f.plan(&mut backend, "new-event");
        assert!(apply(&f.0, next, &mut backend).is_err());
        assert_eq!(
            status(&f.0, &scope(), "1", &plan.operation, &mut backend)
                .unwrap()
                .state,
            "unknown"
        );
        backend.observation.label = "new-7".into();
        backend.observation.group_label = "new-7".into();
        assert_eq!(
            status(&f.0, &scope(), "1", &plan.operation, &mut backend)
                .unwrap()
                .state,
            "verified"
        );
        assert_eq!(backend.calls, 1);
        assert!(status(&f.0, &scope(), "other", &plan.operation, &mut backend).is_err());
    }
    #[test]
    fn collisions_and_recycled_terminal_generations_never_mutate() {
        let f = Fixture::new();
        let mut backend = Mock::new();
        f.con()
            .execute_batch(
                "INSERT INTO agents VALUES(8,'new','new-8');INSERT INTO aliases VALUES('new-7',8);",
            )
            .unwrap();
        assert!(
            Plan::prepare(
                &f.0,
                Request {
                    scope: scope(),
                    actor: "1",
                    target: "old-7",
                    base: "new",
                    event: "event",
                    pane: pane()
                },
                &mut backend
            )
            .is_err()
        );
        f.con()
            .execute("DELETE FROM aliases WHERE token='new-7'", [])
            .unwrap();
        let plan = f.plan(&mut backend, "event");
        backend.observation.terminal.generation = "recycled".into();
        assert!(apply(&f.0, plan, &mut backend).is_err());
        assert_eq!(backend.calls, 0);
        assert_eq!(
            f.con()
                .query_row("SELECT display FROM agents WHERE id=7", [], |r| r
                    .get::<_, String>(0))
                .unwrap(),
            "old-7"
        );
    }
    #[test]
    fn display_names_preserve_decimal_suffix_and_slug_rules() {
        assert_eq!(
            display("Review.Work-123", 123).unwrap(),
            ("Review.Work".into(), "review-work-123".into())
        );
        assert_eq!(display(&"x".repeat(200), 123).unwrap().1.len(), 63);
        assert!(display("two names", 7).is_err());
    }
    #[test]
    fn herdr_metadata_rpc_is_correlated_and_sends_only_identity_tokens() {
        use std::{
            io::{BufRead, BufReader},
            os::unix::net::UnixListener,
        };
        let fixture = Fixture::new();
        let socket = fixture.0.join("herdr.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let thread = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut line = String::new();
            BufReader::new(stream.try_clone().unwrap())
                .read_line(&mut line)
                .unwrap();
            let request: Value = serde_json::from_str(&line).unwrap();
            assert_eq!(request["method"], "pane.report_metadata");
            assert_eq!(
                request["params"]["tokens"],
                json!({"armonia_id":"7","name":"new-7","use":"new-7"})
            );
            writeln!(
                stream,
                "{}",
                json!({"id":request["id"],"result":{"ok":true}})
            )
            .unwrap();
        });
        let terminals = Terminals::default();
        let native = Native {
            terminals: &terminals,
            herdr_sockets: BTreeMap::from([("fixture".into(), socket)]),
        };
        assert_eq!(native.rpc("fixture","pane.report_metadata",json!({"pane_id":"pane","source":"armonia","tokens":{"armonia_id":"7","name":"new-7","use":"new-7"}})).unwrap()["ok"],true);
        thread.join().unwrap();
        assert!(
            native
                .rpc("unconfigured", "pane.report_metadata", json!({}))
                .is_err()
        );
    }
    #[test]
    fn cli_review_is_required_and_local_actor_cannot_be_spoofed() {
        let f = Fixture::new();
        let config = super::super::config::ControlConfig::fresh(&f.0).unwrap();
        layouts::write_private(&f.0.join("control.json"), &config.encode().unwrap()).unwrap();
        let target = f.0.join("target.json");
        layouts::write_private(&target, &serde_json::to_vec(&pane()).unwrap()).unwrap();
        let mut args = vec![
            "preview".into(),
            "--config-dir".into(),
            f.0.to_string_lossy().into_owned(),
            "--target".into(),
            target.to_string_lossy().into_owned(),
            "--agent".into(),
            "old-7".into(),
            "--name".into(),
            "new".into(),
            "--event".into(),
            "fixture".into(),
        ];
        let mut backend = Mock::new();
        let review = execute_cli(&cli(&args).unwrap().unwrap(), &mut backend).unwrap();
        assert_eq!(backend.calls, 0);
        args[0] = "apply".into();
        assert!(cli(&args).is_err());
        args.extend(["--approve".into(), "wrong".into()]);
        assert!(execute_cli(&cli(&args).unwrap().unwrap(), &mut backend).is_err());
        assert_eq!(backend.calls, 0);
        *args.last_mut().unwrap() = review["approval"].as_str().unwrap().into();
        assert_eq!(
            execute_cli(&cli(&args).unwrap().unwrap(), &mut backend).unwrap()["state"],
            "verified"
        );
        args.extend(["--actor".into(), "telegram-admin".into()]);
        assert!(cli(&args).is_err());
        let (scope, actor) = local_scope(&f.0).unwrap();
        assert_eq!(scope.bot, "local-cli");
        assert!(actor.starts_with("uid:"));
    }
}
