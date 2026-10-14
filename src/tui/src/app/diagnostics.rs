//! Read-only, bounded local observability. Output contains aggregate numbers and
//! fixed receipt status strings; payloads and stored errors are never forwarded.
//! The optional organization projection exposes only bounded registry labels.
//! WAL reads use ordinary SQLite SHM reader coordination. No application state,
//! database/WAL contents, or goal/lease recovery state is written.
use super::{config::ControlConfig, hook_runtime::Registration};
use crate::core::layouts;
use rusqlite::{Connection, OpenFlags};
use serde_json::{Value, json};
use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    os::unix::fs::{MetadataExt, OpenOptionsExt},
    path::{Path, PathBuf},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
type Result<T> = std::result::Result<T, String>;
const MAX_DATABASE: u64 = 512 * 1024 * 1024;
const STATES: [&str; 15] = [
    "pending",
    "retryable",
    "intent",
    "accepted",
    "delivered",
    "unknown",
    "blocked",
    "cancelled",
    "received",
    "processing",
    "handled",
    "ignored",
    "failed",
    "queued",
    "unrecognized",
];

fn unknown(reason: &'static str) -> Value {
    json!({"status":"unknown","reason":reason})
}
fn private_directory(path: &Path) -> Result<()> {
    layouts::check_path(path).map_err(|_| "Unsafe diagnostics directory")?;
    let meta = fs::symlink_metadata(path).map_err(|_| "Diagnostics directory is unavailable")?;
    if !meta.is_dir() || meta.uid() != unsafe { libc::geteuid() } || meta.mode() & 0o077 != 0 {
        return Err("Diagnostics requires a private owned directory".into());
    }
    Ok(())
}
fn private_file(path: &Path, limit: u64) -> Result<Option<File>> {
    layouts::check_path(path).map_err(|_| "Unsafe diagnostics state file")?;
    let file = match OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
        .open(path)
    {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err("Diagnostics state file cannot be read".into()),
    };
    let meta = file
        .metadata()
        .map_err(|_| "Diagnostics state metadata unavailable")?;
    if !meta.is_file()
        || meta.uid() != unsafe { libc::geteuid() }
        || meta.nlink() != 1
        || meta.mode() & 0o077 != 0
        || meta.len() > limit
    {
        return Err("Diagnostics requires bounded private owned regular files".into());
    }
    Ok(Some(file))
}
fn read_file(path: &Path, limit: u64) -> Result<Option<Vec<u8>>> {
    let Some(file) = private_file(path, limit)? else {
        return Ok(None);
    };
    let mut bytes = Vec::new();
    file.take(limit + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| "Diagnostics state read failed")?;
    if bytes.len() as u64 > limit {
        return Err("Diagnostics state exceeds limit".into());
    }
    Ok(Some(bytes))
}
fn configuration(directory: &Path) -> Value {
    match read_file(&directory.join("control.json"), 1024 * 1024) {
        Ok(None) => json!({"status":"missing"}),
        Ok(Some(bytes)) => match ControlConfig::parse(&bytes) {
            Ok(_) => json!({"status":"ok"}),
            Err(_) => unknown("invalid_configuration"),
        },
        Err(_) => unknown("unsafe_or_unreadable_configuration"),
    }
}
fn same_file(file: &File, path: &Path) -> bool {
    let (Ok(a), Ok(b)) = (file.metadata(), fs::symlink_metadata(path)) else {
        return false;
    };
    b.is_file() && a.dev() == b.dev() && a.ino() == b.ino()
}
struct Database {
    con: Connection,
    file: File,
    path: PathBuf,
    metadata: fs::Metadata,
    immutable: bool,
}
impl Database {
    fn stable(&self) -> bool {
        let Ok(after) = self.file.metadata() else {
            return false;
        };
        same_file(&self.file, &self.path)
            && (!self.immutable
                || (self.metadata.len() == after.len()
                    && self.metadata.mtime() == after.mtime()
                    && self.metadata.mtime_nsec() == after.mtime_nsec()
                    && !self
                        .path
                        .with_file_name(format!(
                            "{}-wal",
                            self.path.file_name().unwrap().to_string_lossy()
                        ))
                        .exists()))
    }
}
fn open_database(path: &Path) -> Result<Option<Database>> {
    let Some(file) = private_file(path, MAX_DATABASE)? else {
        return Ok(None);
    };
    // Pin and validate every possible SQLite companion before SQLite opens it.
    // A WAL without its shared-memory index is unknown: do not create that index
    // or trigger journal recovery as an observability operation.
    let wal = private_file(
        &path.with_file_name(format!(
            "{}-wal",
            path.file_name().unwrap().to_string_lossy()
        )),
        MAX_DATABASE,
    )?;
    let shm = private_file(
        &path.with_file_name(format!(
            "{}-shm",
            path.file_name().unwrap().to_string_lossy()
        )),
        16 * 1024 * 1024,
    )?;
    let journal = private_file(
        &path.with_file_name(format!(
            "{}-journal",
            path.file_name().unwrap().to_string_lossy()
        )),
        MAX_DATABASE,
    )?;
    if journal.is_some() || (wal.is_some() && shm.is_none()) {
        return Err("Database needs a complete read-only snapshot".into());
    }
    let flags = OpenFlags::SQLITE_OPEN_READ_ONLY
        | OpenFlags::SQLITE_OPEN_NO_MUTEX
        | OpenFlags::SQLITE_OPEN_NOFOLLOW;
    let metadata = file
        .metadata()
        .map_err(|_| "Cannot inspect database metadata")?;
    let immutable = wal.is_none();
    let con = if immutable {
        // SQLite can otherwise create empty WAL/SHM files even for READ_ONLY.
        // Without a WAL, immutable mode reads the main file without side effects;
        // its identity/mtime and absence of a new WAL are rechecked after queries.
        use std::os::unix::ffi::OsStrExt;
        let encoded = path
            .as_os_str()
            .as_bytes()
            .iter()
            .map(|b| {
                if b.is_ascii_alphanumeric() || b"/-_.~".contains(b) {
                    (*b as char).to_string()
                } else {
                    format!("%{b:02X}")
                }
            })
            .collect::<String>();
        Connection::open_with_flags(
            format!("file:{encoded}?immutable=1"),
            flags | OpenFlags::SQLITE_OPEN_URI,
        )
    } else {
        Connection::open_with_flags(path, flags)
    }
    .map_err(|_| "Database cannot be inspected read-only")?;
    if !same_file(&file, path) {
        return Err("Database identity changed".into());
    }
    con.busy_timeout(Duration::from_millis(100))
        .map_err(|_| "Cannot bound database wait")?;
    let started = Instant::now();
    let mut steps = 0usize;
    con.progress_handler(
        1000,
        Some(move || {
            steps += 1;
            steps > 20_000 || started.elapsed() > Duration::from_secs(2)
        }),
    )
    .map_err(|_| "Cannot bound database execution")?;
    con.execute_batch("PRAGMA query_only=ON; PRAGMA trusted_schema=OFF; BEGIN;")
        .map_err(|_| "Cannot start read-only diagnostics")?;
    Ok(Some(Database {
        con,
        file,
        path: path.into(),
        metadata,
        immutable,
    }))
}
/// Share the bounded read-only connection without schema creation or recovery.
pub(crate) fn with_database<T>(
    path: &Path,
    read: impl FnOnce(&Connection) -> Result<T>,
) -> Result<Option<T>> {
    let parent = path.parent().ok_or("Database path has no parent")?;
    private_directory(parent)?;
    let Some(database) = open_database(path)? else {
        return Ok(None);
    };
    let result = read(&database.con)?;
    if !database.stable() {
        return Err("Database changed during read".into());
    }
    Ok(Some(result))
}
fn real_table(con: &Connection, name: &str) -> rusqlite::Result<bool> {
    con.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type='table' AND name=? AND rootpage>0)",
        [name],
        |r| r.get(0),
    )
}
fn aggregate(con: &Connection, table: &str, now: f64) -> rusqlite::Result<Value> {
    if !real_table(con, table)? {
        return Err(rusqlite::Error::InvalidQuery);
    }
    // table is selected by this module, never by a CLI option or stored value.
    let whitelist = STATES[..STATES.len() - 1]
        .iter()
        .map(|s| format!("'{s}'"))
        .collect::<Vec<_>>()
        .join(",");
    let sql = format!(
        "SELECT CASE WHEN state IN ({whitelist}) THEN state ELSE 'unrecognized' END AS bucket,COUNT(*),MIN(created_at),SUM(CASE WHEN error IS NOT NULL AND error<>'' THEN 1 ELSE 0 END) FROM {table} GROUP BY bucket"
    );
    let mut statement = con.prepare(&sql)?;
    let rows = statement.query_map([], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, i64>(1)?,
            r.get::<_, Option<f64>>(2)?,
            r.get::<_, i64>(3)?,
        ))
    })?;
    let mut states = serde_json::Map::new();
    for state in STATES {
        states.insert(
            state.into(),
            json!({"count":0,"oldest_age_seconds":null,"errors":0}),
        );
    }
    let mut total = 0i64;
    for row in rows {
        let (state, count, created, errors) = row?;
        let age = match created {
            Some(t) if t.is_finite() && t >= 0.0 => Some((now - t).max(0.0)),
            None => None,
            _ => return Err(rusqlite::Error::InvalidQuery),
        };
        if !states.contains_key(&state) {
            return Err(rusqlite::Error::InvalidQuery);
        }
        total = total
            .checked_add(count)
            .ok_or(rusqlite::Error::InvalidQuery)?;
        states.insert(
            state,
            json!({"count":count,"oldest_age_seconds":age,"errors":errors}),
        );
    }
    Ok(json!({"status":"ok","total":total,"states":states}))
}
fn cursors(con: &Connection) -> rusqlite::Result<Value> {
    if !real_table(con, "cursors")? {
        return Err(rusqlite::Error::InvalidQuery);
    }
    // Reading offset validates the expected column without exposing identifiers.
    let(count,invalid):(i64,i64)=con.query_row("SELECT COUNT(*),COALESCE(SUM(CASE WHEN typeof(offset)<>'integer' OR offset<0 THEN 1 ELSE 0 END),0) FROM cursors",[],|r|Ok((r.get(0)?,r.get(1)?)))?;
    if invalid > 0 {
        return Err(rusqlite::Error::InvalidQuery);
    }
    Ok(json!({"status":"ok","present":count>0,"count":count}))
}
fn delivery(directory: &Path, now: f64) -> Value {
    match open_database(&directory.join("delivery.sqlite3")) {
        Ok(None) => json!({"status":"missing","delivery":null,"inbox":null,"cursors":null}),
        Err(_) => {
            json!({"status":"unknown","reason":"unsafe_unreadable_or_incomplete_database","delivery":null,"inbox":null,"cursors":null})
        }
        Ok(Some(database)) => {
            let con = &database.con;
            let delivery = aggregate(con, "delivery", now)
                .unwrap_or_else(|_| unknown("schema_query_or_budget_failure"));
            let inbox = aggregate(con, "inbox", now)
                .unwrap_or_else(|_| unknown("schema_query_or_budget_failure"));
            let cursors =
                cursors(con).unwrap_or_else(|_| unknown("schema_query_or_budget_failure"));
            if !database.stable() {
                return json!({"status":"unknown","reason":"database_changed_during_inspection","delivery":null,"inbox":null,"cursors":null});
            }
            let status = if [&delivery, &inbox, &cursors]
                .iter()
                .all(|v| v["status"] == "ok")
            {
                "ok"
            } else {
                "unknown"
            };
            json!({"status":status,"delivery":delivery,"inbox":inbox,"cursors":cursors})
        }
    }
}
/// Private registration inventory for local control. This is never included in
/// the shareable diagnostics report, which exposes counts only.
pub(crate) fn registrations(directory: &Path) -> Result<Vec<Registration>> {
    let root = directory.join("hooks");
    if fs::symlink_metadata(&root).is_err_and(|e| e.kind() == std::io::ErrorKind::NotFound) {
        return Ok(vec![]);
    }
    private_directory(&root)?;
    let mut registrations = Vec::new();
    let mut total = 0usize;
    for (index, entry) in fs::read_dir(&root)
        .map_err(|_| "Cannot inspect hook registrations")?
        .enumerate()
    {
        if index >= 1000 {
            return Err("Hook registration limit reached".into());
        }
        let path = entry
            .map_err(|_| "Cannot inspect hook registration")?
            .path();
        if path.extension().and_then(|s| s.to_str()) != Some("json") {
            continue;
        }
        let bytes = read_file(&path, 256 * 1024)?.ok_or("Hook registration disappeared")?;
        total += bytes.len();
        if total > 16 * 1024 * 1024 {
            return Err("Hook registration size limit reached".into());
        }
        let registration: Registration =
            serde_json::from_slice(&bytes).map_err(|_| "Invalid hook registration")?;
        registration
            .validate()
            .map_err(|_| "Invalid hook registration")?;
        if registration.directory != directory || registration.path() != path {
            return Err("Hook registration belongs to another instance".into());
        }
        registrations.push(registration);
    }
    registrations.sort_by(|a, b| a.id.cmp(&b.id));
    Ok(registrations)
}
pub(crate) fn goal_rows(
    directory: &Path,
    instance: &str,
    registrations: &[Registration],
    now: u64,
) -> Result<Vec<Value>> {
    Ok(with_database(&directory.join("goals.sqlite3"),|con|{
        if !real_table(con,"goal_records").map_err(|_|"Cannot inspect goal schema")?{return Err("Goal schema unavailable".into());}
        let mut query=con.prepare("SELECT goal_id,scope,goal,epoch FROM goal_records WHERE instance=? LIMIT 1001").map_err(|_|"Cannot read goal records")?;
        let rows=query.query_map([instance],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,String>(2)?,r.get::<_,i64>(3)?))).map_err(|_|"Cannot read goal records")?;
        let mut result=Vec::new();
        for row in rows {
            if result.len()>=1000{return Err("Goal limit reached".into());}
            let (id,scope,goal,epoch)=row.map_err(|_|"Invalid goal record")?;
            if scope.len()>16384 || goal.len()>128*1024 || id.len()>128 || epoch<0{return Err("Invalid goal record bounds".into());}
            let scope:crate::core::goal_store::Scope=serde_json::from_str(&scope).map_err(|_|"Invalid goal scope")?;
            scope.validate().map_err(|_|"Invalid goal scope")?;
            let mut goal:crate::core::goals::Goal=serde_json::from_str(&goal).map_err(|_|"Invalid goal state")?;
            let registered=registrations.iter().find(|r|r.goal_id.as_deref()==Some(&id) && r.scope==scope);
            let timeout=registered.map_or(86400000,|r|r.pending_timeout_ms);
            // Expire a projection only. Observation must never recover or arm work.
            goal.expire(now,timeout);
            let status=serde_json::to_value(&goal.status).map_err(|_|"Invalid goal status")?;
            result.push(json!({"name":id,"id":id,"scope":scope,"epoch":epoch,"directive":goal.directive,"status":status,"registered":registered.is_some()}));
        }
        Ok(result)
    })?.unwrap_or_default())
}
pub(crate) fn hook_control_view(directory: &Path, instance: &str, now: u64) -> Result<Value> {
    let registrations = registrations(directory)?;
    let mut tools = Vec::new();
    let mut switches = Vec::new();
    for capability in super::hook_install::capabilities() {
        let matching = registrations
            .iter()
            .filter(|r| {
                serde_json::to_value(r.scope.harness).ok() == Some(json!(capability.harness))
            })
            .collect::<Vec<_>>();
        let files = matching
            .iter()
            .map(|r| json!({"path":r.path(),"exists":true}))
            .collect::<Vec<_>>();
        let events = matching
            .iter()
            .flat_map(|r| r.events.iter())
            .collect::<Vec<_>>();
        tools.push(json!({"tool":capability.harness,"name":capability.harness,"target":directory.join("hooks"),"files":files,"events":events,"labels":[[if matching.is_empty(){"Not configured"}else{"Registered · runtime unverified"},if matching.is_empty(){""}else{"warn"}]],"reference":capability.reference,"integration":capability.integration}));
    }
    for registration in &registrations {
        for (key, title, on) in [
            (
                "telegram_delivery",
                "Telegram delivery",
                registration.options.telegram_delivery,
            ),
            (
                "telegram_progress",
                "Telegram progress",
                registration.options.telegram_progress,
            ),
            (
                "notifications",
                "Notifications",
                registration.options.notifications,
            ),
            (
                "hindsight",
                "Hindsight memory",
                registration.options.hindsight,
            ),
        ] {
            switches.push(json!({"key":format!("{}:{key}",registration.id),"title":format!("{} · {title}",registration.id),"on":on,"kind":"feature","effect":"Applies only to this verified hook registration at its next event."}));
        }
    }
    let goals = goal_rows(directory, instance, &registrations, now);
    Ok(
        json!({"directory":directory,"tools":tools,"switches":switches,"goals":goals.as_ref().ok(),"goal_status":if goals.is_ok(){"ok"}else{"unknown"},"registrations":registrations}),
    )
}
fn hooks(directory: &Path) -> Value {
    let root = directory.join("hooks");
    if fs::symlink_metadata(&root).is_err_and(|e| e.kind() == std::io::ErrorKind::NotFound) {
        return json!({"status":"missing","runtime":"not_probed"});
    }
    if private_directory(&root).is_err() {
        return unknown("unsafe_hook_directory");
    }
    let Ok(entries) = fs::read_dir(&root) else {
        return unknown("unreadable_hook_directory");
    };
    let (mut valid, mut invalid, mut goals, mut bytes_read, mut count) =
        (0u64, 0u64, 0u64, 0usize, 0usize);
    for entry in entries {
        count += 1;
        if count > 1000 {
            return unknown("hook_scan_limit");
        }
        let Ok(entry) = entry else {
            return unknown("hook_scan_failed");
        };
        if entry.path().extension().and_then(|s| s.to_str()) != Some("json") {
            continue;
        }
        let bytes = match read_file(&entry.path(), 256 * 1024) {
            Ok(Some(b)) => b,
            _ => return unknown("unsafe_or_unreadable_hook_registration"),
        };
        bytes_read += bytes.len();
        if bytes_read > 16 * 1024 * 1024 {
            return unknown("hook_scan_limit");
        }
        match serde_json::from_slice::<Registration>(&bytes) {
            Ok(r)
                if r.directory == directory && r.path() == entry.path() && r.validate().is_ok() =>
            {
                valid += 1;
                goals += u64::from(r.goal_id.is_some());
            }
            _ => invalid += 1,
        }
    }
    json!({"status":if invalid==0{"ok"}else{"unknown"},"valid_registrations":valid,"invalid_registrations":invalid,"goal_enabled_registrations":goals,"runtime":"not_probed"})
}
/// Optional Armonia registry projection. This never opens the registry through
/// a service constructor or applies an organization plan.
pub fn organization(directory: &Path, now: f64) -> Result<Value> {
    private_directory(directory)?;
    if !now.is_finite() || now < 0.0 {
        return Err("Invalid organization time".into());
    }
    let database = match open_database(&directory.join("armonia.sqlite3")) {
        Ok(Some(database)) => database,
        Ok(None) => return Ok(json!({"status":"missing","apply":false,"entries":[]})),
        Err(_) => return Ok(unknown("unsafe_or_unreadable_registry")),
    };
    let query = || -> rusqlite::Result<Value> {
        if !real_table(&database.con, "agents")? || !real_table(&database.con, "bindings")? {
            return Err(rusqlite::Error::InvalidQuery);
        }
        let mut statement=database.con.prepare("SELECT a.id,a.display,a.manual_category,a.pin_rank,a.last_human,b.workspace,b.availability FROM agents a JOIN bindings b ON b.agent_id=a.id LIMIT 10001")?;
        let rows = statement.query_map([], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, Option<String>>(2)?,
                r.get::<_, Option<i64>>(3)?,
                r.get::<_, Option<f64>>(4)?,
                r.get::<_, String>(5)?,
                r.get::<_, String>(6)?,
            ))
        })?;
        let mut entries = Vec::new();
        for row in rows {
            let (id, display, manual, pin, last, workspace, availability) = row?;
            if entries.len() >= 10000
                || [&display, &workspace, manual.as_deref().unwrap_or("")]
                    .iter()
                    .any(|s| s.len() > 1024 || s.chars().any(char::is_control))
                || last.is_some_and(|v| !v.is_finite())
            {
                return Err(rusqlite::Error::InvalidQuery);
            }
            let (category, origin, rank) = if let Some(category) = manual.filter(|s| !s.is_empty())
            {
                (category, "manual", 2)
            } else if last.is_some_and(|stamp| now - stamp <= 604800.0) {
                ("Active".into(), "human_activity", 0)
            } else {
                ("Sessions".into(), "automatic_default", 1)
            };
            entries.push((workspace.clone(),rank,category.clone(),pin.is_none(),pin.unwrap_or(0),last.unwrap_or(0.0),id,json!({"id":id,"display":display,"workspace":workspace,"category":category,"category_origin":origin,"pin_rank":pin,"availability":if availability=="live"{"live"}else{"not_running"}})));
        }
        entries.sort_by(|a, b| {
            (&a.0, a.1, &a.2, a.3, a.4)
                .cmp(&(&b.0, b.1, &b.2, b.3, b.4))
                .then_with(|| b.5.total_cmp(&a.5))
                .then(a.6.cmp(&b.6))
        });
        Ok(
            json!({"status":"ok","apply":false,"entries":entries.into_iter().map(|r|r.7).collect::<Vec<_>>(),"note":"Read-only registry order. Organizer policy and exact terminal verification are required before any changes. Stored availability may be stale."}),
        )
    };
    let result = query();
    if !database.stable() {
        return Ok(unknown("registry_changed_during_read"));
    }
    Ok(result.unwrap_or_else(|_| unknown("invalid_or_unavailable_registry_schema")))
}
/// Bounded receipt timeline without payloads, destinations or stored error text.
pub fn recent(directory: &Path) -> Result<Value> {
    private_directory(directory)?;
    let database = match open_database(&directory.join("delivery.sqlite3")) {
        Ok(Some(database)) => database,
        Ok(None) => return Ok(json!({"status":"missing","events":[]})),
        Err(_) => return Ok(unknown("unsafe_or_unreadable_journal")),
    };
    let query = || -> rusqlite::Result<Value> {
        if !real_table(&database.con, "delivery")? || !real_table(&database.con, "inbox")? {
            return Err(rusqlite::Error::InvalidQuery);
        }
        let mut events = Vec::new();
        for table in ["delivery", "inbox"] {
            let mut statement=database.con.prepare(&format!("SELECT id,state,created_at,CASE WHEN error IS NOT NULL AND error<>'' THEN 1 ELSE 0 END FROM {table} ORDER BY id DESC LIMIT 50"))?;
            let rows = statement.query_map([], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, f64>(2)?,
                    row.get::<_, i64>(3)?,
                ))
            })?;
            for row in rows {
                let (id, state, time, error) = row?;
                if id < 0 || !time.is_finite() || time < 0.0 {
                    return Err(rusqlite::Error::InvalidQuery);
                }
                events.push(json!({"source":table,"id":id,"state":if STATES.contains(&state.as_str()){state.as_str()}else{"unrecognized"},"created_at":time,"has_error":error!=0}));
            }
        }
        events.sort_by(|a, b| {
            b["created_at"]
                .as_f64()
                .unwrap()
                .total_cmp(&a["created_at"].as_f64().unwrap())
        });
        events.truncate(50);
        Ok(json!({"status":"ok","events":events,"limit":50}))
    };
    let result = query();
    if !database.stable() {
        return Ok(unknown("journal_changed_during_read"));
    }
    Ok(result.unwrap_or_else(|_| unknown("invalid_or_unavailable_journal_schema")))
}
/// No service constructors are used: those constructors may recover leases or
/// create schemas. Missing observations remain explicitly missing/unknown.
pub fn inspect(directory: &Path, now: f64) -> Result<Value> {
    private_directory(directory)?;
    if !now.is_finite() || now < 0.0 {
        return Err("Invalid diagnostics time".into());
    }
    Ok(
        json!({"version":1,"configuration":configuration(directory),"journal":delivery(directory,now),"hooks":hooks(directory),"live_probes":false,"goal_recovery":false}),
    )
}
pub fn run(args: &[String], writer: &mut impl Write) -> Result<()> {
    if args == ["--help"] || args == ["-h"] {
        return writeln!(writer,"Usage: daedalus diagnostics --config-dir ABS --json\nRead-only local aggregate diagnostics; no network or terminal probes.").map_err(|_|"Cannot write diagnostics".into());
    }
    let (mut directory, mut json_output) = (None, false);
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--json" if !json_output => json_output = true,
            "--config-dir" if directory.is_none() => {
                index += 1;
                directory = Some(PathBuf::from(
                    args.get(index).ok_or("Missing configuration directory")?,
                ));
            }
            _ => return Err("Unknown or repeated diagnostics option".into()),
        }
        index += 1;
    }
    if !json_output {
        return Err("Use diagnostics --config-dir ABS --json".into());
    }
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| "Invalid system time")?
        .as_secs_f64();
    let report = inspect(&directory.ok_or("Missing --config-dir")?, now)?;
    serde_json::to_writer(&mut *writer, &report).map_err(|_| "Cannot write diagnostics")?;
    writeln!(writer).map_err(|_| "Cannot write diagnostics".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        collections::BTreeMap,
        os::unix::fs::{PermissionsExt, symlink},
        sync::atomic::{AtomicU64, Ordering},
    };
    static NEXT: AtomicU64 = AtomicU64::new(0);
    struct Fixture(PathBuf);
    impl Fixture {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "daedalus-diagnostics-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir(&path).unwrap();
            fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
            Self(path)
        }
        fn file(&self, name: &str, bytes: &[u8]) {
            fs::write(self.0.join(name), bytes).unwrap();
            fs::set_permissions(self.0.join(name), fs::Permissions::from_mode(0o600)).unwrap();
        }
        fn database(&self) -> Connection {
            let con = Connection::open(self.0.join("delivery.sqlite3")).unwrap();
            fs::set_permissions(
                self.0.join("delivery.sqlite3"),
                fs::Permissions::from_mode(0o600),
            )
            .unwrap();
            con.execute_batch("CREATE TABLE delivery(state TEXT,created_at REAL,error TEXT,payload TEXT); CREATE TABLE inbox(state TEXT,created_at REAL,error TEXT,payload TEXT); CREATE TABLE cursors(channel TEXT,bot TEXT,offset INTEGER);").unwrap();
            con
        }
        fn bytes(&self) -> BTreeMap<String, Vec<u8>> {
            fs::read_dir(&self.0)
                .unwrap()
                .map(|r| r.unwrap())
                .filter(|r| r.file_type().unwrap().is_file())
                .map(|r| {
                    (
                        r.file_name().to_string_lossy().into_owned(),
                        fs::read(r.path()).unwrap(),
                    )
                })
                .collect()
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
    #[test]
    fn pending_unknown_blocked_remain_distinct_and_read_only() {
        let f = Fixture::new();
        let con = f.database();
        con.execute_batch("INSERT INTO delivery VALUES('pending',70,NULL,'PRIVATE-PAYLOAD'),('unknown',80,'PRIVATE-TOKEN','PRIVATE-DIRECTIVE'),('blocked',90,'https://private-provider','PRIVATE-CHAT'),('PRIVATE-STATE-SECRET',90,NULL,NULL); INSERT INTO inbox VALUES('processing',95,NULL,'PRIVATE-INBOUND'); INSERT INTO cursors VALUES('telegram','PRIVATE-BOT',123);").unwrap();
        drop(con);
        let before = f.bytes();
        let report = inspect(&f.0, 100.0).unwrap();
        assert_eq!(report["journal"]["status"], "ok");
        for (state, age) in [("pending", 30.0), ("unknown", 20.0), ("blocked", 10.0)] {
            assert_eq!(report["journal"]["delivery"]["states"][state]["count"], 1);
            assert_eq!(
                report["journal"]["delivery"]["states"][state]["oldest_age_seconds"],
                age
            );
        }
        assert_eq!(
            report["journal"]["delivery"]["states"]["unrecognized"]["count"],
            1
        );
        assert_eq!(
            report["journal"]["delivery"]["states"]["blocked"]["errors"],
            1
        );
        assert_eq!(
            report["journal"]["inbox"]["states"]["processing"]["count"],
            1
        );
        assert_eq!(report["journal"]["cursors"]["present"], true);
        assert_eq!(report["journal"]["cursors"]["count"], 1);
        assert!(!report.to_string().contains("PRIVATE"));
        assert!(!report.to_string().contains("https://"));
        assert!(
            before == f.bytes(),
            "Diagnostics must not change state bytes or create files"
        );
        assert!(!f.0.join("goals.sqlite3").exists());
    }
    #[test]
    fn missing_or_malformed_schema_is_unknown_not_zero_and_errors_are_sanitized() {
        let f = Fixture::new();
        let con = Connection::open(f.0.join("delivery.sqlite3")).unwrap();
        fs::set_permissions(
            f.0.join("delivery.sqlite3"),
            fs::Permissions::from_mode(0o600),
        )
        .unwrap();
        con.execute_batch("CREATE TABLE delivery(secret TEXT); CREATE VIEW inbox AS SELECT 'SECRET-CONTENT' AS state;").unwrap();
        drop(con);
        let report = inspect(&f.0, 100.0).unwrap();
        for table in ["delivery", "inbox", "cursors"] {
            assert_eq!(report["journal"][table]["status"], "unknown");
            assert!(report["journal"][table].get("total").is_none());
        }
        f.file("delivery.sqlite3", b"PRIVATE SQL malformed contents");
        f.file(
            "control.json",
            b"{\"agent_name\":\"PRIVATE SECRET\" malformed",
        );
        let report = inspect(&f.0, 100.0).unwrap();
        assert_eq!(report["configuration"]["status"], "unknown");
        assert_eq!(report["journal"]["status"], "unknown");
        assert!(!report.to_string().contains("PRIVATE"));
    }
    #[test]
    fn missing_state_creates_nothing_and_parser_never_echoes_arguments() {
        let f = Fixture::new();
        let before = f.bytes();
        let mut output = Vec::new();
        run(
            &[
                "--config-dir".into(),
                f.0.to_string_lossy().into(),
                "--json".into(),
            ],
            &mut output,
        )
        .unwrap();
        let report: Value = serde_json::from_slice(&output).unwrap();
        assert_eq!(report["journal"]["status"], "missing");
        assert_eq!(report["hooks"]["status"], "missing");
        assert!(
            before == f.bytes(),
            "Diagnostics must not change state bytes or create files"
        );
        assert!(
            !run(&["PRIVATE-ARGUMENT".into()], &mut output)
                .unwrap_err()
                .contains("PRIVATE")
        );
    }
    #[test]
    fn unsafe_files_and_symlink_parents_are_not_read() {
        let f = Fixture::new();
        let target = Fixture::new();
        target.file("secret", b"PRIVATE SECRET");
        symlink(target.0.join("secret"), f.0.join("control.json")).unwrap();
        symlink(target.0.join("secret"), f.0.join("delivery.sqlite3")).unwrap();
        let report = inspect(&f.0, 100.0).unwrap();
        assert_eq!(report["configuration"]["status"], "unknown");
        assert_eq!(report["journal"]["status"], "unknown");
        assert!(!report.to_string().contains("PRIVATE"));
        let link = f.0.join("linked");
        symlink(&target.0, &link).unwrap();
        assert!(inspect(&link, 100.0).is_err());
        fs::set_permissions(&target.0, fs::Permissions::from_mode(0o755)).unwrap();
        assert!(inspect(&target.0, 100.0).is_err());
    }
    #[test]
    fn hook_registration_errors_are_counts_only_without_recovery() {
        let f = Fixture::new();
        fs::create_dir(f.0.join("hooks")).unwrap();
        fs::set_permissions(f.0.join("hooks"), fs::Permissions::from_mode(0o700)).unwrap();
        f.file("hooks/broken.json", b"PRIVATE GOAL DIRECTIVE");
        let report = inspect(&f.0, 100.0).unwrap();
        assert_eq!(report["hooks"]["invalid_registrations"], 1);
        assert_eq!(report["hooks"]["runtime"], "not_probed");
        assert_eq!(report["goal_recovery"], false);
        assert!(!report.to_string().contains("PRIVATE"));
        assert!(!f.0.join("goals.sqlite3").exists());
    }
    #[test]
    fn wal_read_has_current_counts_without_recovery_or_database_writes() {
        let f = Fixture::new();
        let con = f.database();
        con.execute_batch("PRAGMA journal_mode=WAL; INSERT INTO delivery VALUES('blocked',90,NULL,'PRIVATE WAL PAYLOAD');").unwrap();
        let before = f.bytes();
        let report = inspect(&f.0, 100.0).unwrap();
        assert_eq!(
            report["journal"]["delivery"]["states"]["blocked"]["count"],
            1
        );
        let after = f.bytes();
        assert_eq!(
            before.keys().collect::<Vec<_>>(),
            after.keys().collect::<Vec<_>>()
        );
        // SQLite may update SHM reader bookkeeping while acquiring a consistent
        // read transaction. The database and committed WAL remain byte-identical.

        for name in ["delivery.sqlite3", "delivery.sqlite3-wal"] {
            assert_eq!(before.get(name), after.get(name));
        }
        drop(con);
        let before = f.bytes();
        let report = inspect(&f.0, 100.0).unwrap();
        assert!(["ok", "unknown"].contains(&report["journal"]["status"].as_str().unwrap()));
        assert!(
            before == f.bytes(),
            "Diagnostics must not change state bytes or create files"
        );
    }
    #[test]
    fn expensive_query_is_interrupted_and_changed_snapshot_is_rejected() {
        let f = Fixture::new();
        drop(f.database());
        let database = open_database(&f.0.join("delivery.sqlite3"))
            .unwrap()
            .unwrap();
        assert!(database.stable());
        let result=database.con.query_row("WITH RECURSIVE n(x) AS (VALUES(0) UNION ALL SELECT x+1 FROM n WHERE x<1000000000) SELECT SUM(x) FROM n",[],|r|r.get::<_,i64>(0));
        assert!(result.is_err());
        let con = Connection::open(f.0.join("delivery.sqlite3")).unwrap();
        con.execute("INSERT INTO delivery VALUES('pending',90,NULL,NULL)", [])
            .unwrap();
        assert!(!database.stable());
    }
    #[test]
    fn valid_private_configuration_and_hook_identity_never_enter_output() {
        let f = Fixture::new();
        let mut config = ControlConfig::fresh(Path::new("/PRIVATE-WORKSPACE")).unwrap();
        config.agent_name = "PRIVATE-AGENT".into();
        f.file("control.json", &config.encode().unwrap());
        fs::create_dir(f.0.join("hooks")).unwrap();
        fs::set_permissions(f.0.join("hooks"), fs::Permissions::from_mode(0o700)).unwrap();
        let registration = Registration {
            version: 1,
            id: "PRIVATE-HOOK".into(),
            directory: f.0.clone(),
            scope: crate::core::goal_store::Scope {
                instance: "PRIVATE-INSTANCE".into(),
                terminal: crate::core::identity::TerminalId {
                    backend: crate::core::identity::Backend::Herdr,
                    endpoint: "PRIVATE-ENDPOINT".into(),
                    pane: "PRIVATE-PANE".into(),
                    generation: "PRIVATE-GENERATION".into(),
                },
                conversation: "PRIVATE-SESSION".into(),
                harness: super::super::hooks::Harness::Claude,
            },
            goal_id: Some("PRIVATE-GOAL".into()),
            harness_process: None,
            server_emitter: None,
            transcript: None,
            events: vec![super::super::hooks::EventKind::Stop],
            options: super::super::hook_runtime::Options::default(),
            activity_lease_ms: 30000,
            pending_timeout_ms: 30000,
            hook_timeout_ms: 12000,
        };
        f.file(
            "hooks/PRIVATE-HOOK.json",
            &serde_json::to_vec(&registration).unwrap(),
        );
        let report = inspect(&f.0, 100.0).unwrap();
        assert_eq!(report["configuration"]["status"], "ok");
        assert_eq!(report["hooks"]["status"], "ok");
        assert_eq!(report["hooks"]["valid_registrations"], 1);
        assert_eq!(report["hooks"]["goal_enabled_registrations"], 1);
        assert!(!report.to_string().contains("PRIVATE"));
        assert!(!f.0.join("goals.sqlite3").exists());
    }
    #[test]
    fn recent_receipts_are_bounded_private_and_never_recover_or_leak() {
        let f = Fixture::new();
        let con = Connection::open(f.0.join("delivery.sqlite3")).unwrap();
        fs::set_permissions(
            f.0.join("delivery.sqlite3"),
            fs::Permissions::from_mode(0o600),
        )
        .unwrap();
        con.execute_batch("CREATE TABLE delivery(id INTEGER PRIMARY KEY,state TEXT,created_at REAL,error TEXT,payload TEXT); CREATE TABLE inbox(id INTEGER PRIMARY KEY,state TEXT,created_at REAL,error TEXT,payload TEXT);").unwrap();
        for i in 0..70 {
            con.execute(
                "INSERT INTO delivery VALUES(?,'unknown',?,'PRIVATE-TOKEN','PRIVATE-PAYLOAD')",
                rusqlite::params![i, i as f64],
            )
            .unwrap();
        }
        con.execute(
            "INSERT INTO inbox VALUES(1,'PRIVATE-STATE',100,'PRIVATE-ERROR','PRIVATE-MESSAGE')",
            [],
        )
        .unwrap();
        drop(con);
        let before = f.bytes();
        let report = recent(&f.0).unwrap();
        assert_eq!(report["status"], "ok");
        assert_eq!(report["events"].as_array().unwrap().len(), 50);
        assert_eq!(report["events"][0]["state"], "unrecognized");
        assert!(!report.to_string().contains("PRIVATE"));
        assert_eq!(before, f.bytes());
    }
    #[test]
    fn optional_organization_is_readonly_ordered_and_missing_is_not_empty_success() {
        let f = Fixture::new();
        assert_eq!(organization(&f.0, 1000000.0).unwrap()["status"], "missing");
        let con = Connection::open(f.0.join("armonia.sqlite3")).unwrap();
        fs::set_permissions(
            f.0.join("armonia.sqlite3"),
            fs::Permissions::from_mode(0o600),
        )
        .unwrap();
        con.execute_batch("CREATE TABLE agents(id INTEGER,display TEXT,manual_category TEXT,pin_rank INTEGER,last_human REAL); CREATE TABLE bindings(agent_id INTEGER,workspace TEXT,availability TEXT); INSERT INTO agents VALUES(1,'old',NULL,NULL,1),(2,'active',NULL,NULL,999999),(3,'manual','Review',1,NULL); INSERT INTO bindings VALUES(1,'project','unavailable'),(2,'project','live'),(3,'project','live');").unwrap();
        drop(con);
        let before = f.bytes();
        let report = organization(&f.0, 1000000.0).unwrap();
        assert_eq!(report["status"], "ok");
        assert_eq!(report["apply"], false);
        assert_eq!(report["entries"][0]["display"], "active");
        assert_eq!(report["entries"][1]["availability"], "not_running");
        assert_eq!(report["entries"][2]["category"], "Review");
        assert_eq!(before, f.bytes());
    }
    #[test]
    fn goal_projection_expires_lease_without_mutating_database() {
        let f = Fixture::new();
        let path = f.0.join("goals.sqlite3");
        let connection = super::super::telegram_config::private_connection(&path).unwrap();
        let mut store = crate::core::goal_store::Store::attach_connection(connection).unwrap();
        let scope:crate::core::goal_store::Scope=serde_json::from_value(json!({"instance":"fixture","terminal":{"backend":"tmux","endpoint":"/fixture.sock","pane":"%1","generation":"generation"},"harness":"claude","conversation":"session"})).unwrap();
        let mut record = store.arm("goal", &scope, "Finish fixture", 0).unwrap();
        record.goal.status = crate::core::goals::Status::Running { lease_until: 10 };
        let connection = store.into_connection();
        connection
            .execute(
                "UPDATE goal_records SET goal=?",
                [serde_json::to_string(&record.goal).unwrap()],
            )
            .unwrap();
        drop(connection);
        let before = fs::read(&path).unwrap();
        let rows = goal_rows(&f.0, "fixture", &[], 20).unwrap();
        assert_eq!(rows[0]["status"]["state"], "blocked");
        assert_eq!(fs::read(&path).unwrap(), before);
        assert!(goal_rows(&f.0, "other", &[], 20).unwrap().is_empty());
        let report = inspect(&f.0, 0.02).unwrap().to_string();
        assert!(!report.contains("Finish fixture"));
    }
}
