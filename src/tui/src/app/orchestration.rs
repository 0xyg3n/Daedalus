//! Durable task intent and dependency scheduling for exact native agent panes.
//! A dispatch interrupted before its outcome is recorded is never replayed.
use crate::{
    adapters::{
        terminal_actions::{Action, Receipt, ReceiptState},
        terminals::{self, Terminals},
    },
    core::identity::TerminalId,
};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeSet,
    fs,
    path::{Path, PathBuf},
    time::{Duration, SystemTime, UNIX_EPOCH},
};
type Result<T> = std::result::Result<T, String>;
fn error(e: impl std::fmt::Display) -> String {
    e.to_string()
}
pub fn now() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64()
}
pub fn new_id() -> String {
    format!(
        "{:x}-{:x}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
    )
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Task {
    pub key: String,
    pub label: String,
    pub pane: Value,
    pub identity: TerminalId,
    pub agent: String,
    pub session: Option<String>,
    pub prompt: String,
    pub dependencies: Vec<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Plan {
    pub id: String,
    pub name: String,
    pub parallel: usize,
    pub tasks: Vec<Task>,
}
impl Plan {
    pub fn validate(&self) -> Result<()> {
        if self.id.is_empty()
            || self.id.len() > 100
            || !self.id.bytes().all(|b| b.is_ascii_hexdigit() || b == b'-')
            || self.name.trim().is_empty()
            || self.name.len() > 160
            || self.name.chars().any(char::is_control)
            || !(1..=8).contains(&self.parallel)
            || self.tasks.is_empty()
            || self.tasks.len() > 32
        {
            return Err("Use a named plan with 1–32 tasks and 1–8 parallel agents".into());
        }
        let mut keys = BTreeSet::new();
        for task in &self.tasks {
            task.identity.validate().map_err(error)?;
            if task.key.is_empty()
                || task.key.len() > 40
                || !task
                    .key
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"_-".contains(&b))
                || !keys.insert(task.key.clone())
                || task.label.trim().is_empty()
                || task.label.len() > 160
                || task.label.chars().any(char::is_control)
                || task.prompt.trim().len() < 16
                || task.prompt.len() > 16000
                || task
                    .prompt
                    .chars()
                    .any(|c| c.is_control() && c != '\n' && c != '\t')
                || task.agent.is_empty()
                || crate::adapters::harnesses::canonical_id(&task.agent).is_none()
                || task.session.as_deref().is_none_or(|s| {
                    s.is_empty() || s.len() > 4096 || s.chars().any(char::is_control)
                })
                || task.dependencies.len() > 32
                || terminals::identity(&task.pane)? != task.identity
            {
                return Err("Invalid task, prompt or exact terminal identity".into());
            }
        }
        let mut settled = BTreeSet::new();
        loop {
            let before = settled.len();
            for task in &self.tasks {
                if task
                    .dependencies
                    .iter()
                    .any(|key| !keys.contains(key) || key == &task.key)
                {
                    return Err("Dependency references an unknown task or itself".into());
                }
                if task.dependencies.iter().all(|key| settled.contains(key)) {
                    settled.insert(task.key.clone());
                }
            }
            if settled.len() == self.tasks.len() {
                break;
            }
            if before == settled.len() {
                return Err("Task dependencies contain a cycle".into());
            }
        }
        Ok(())
    }
    pub fn fingerprint(&self) -> Result<String> {
        self.validate()?;
        Ok(format!(
            "{:x}",
            Sha256::digest(serde_json::to_vec(self).map_err(error)?)
        ))
    }
}
#[derive(Clone, Debug, Serialize)]
pub struct Row {
    pub plan: String,
    pub task: Task,
    pub state: String,
    pub submitted: f64,
}
pub struct Store {
    db: Connection,
}
impl Store {
    pub fn memory() -> Result<Self> {
        Self::init(Connection::open_in_memory().map_err(error)?)
    }
    fn init(db: Connection) -> Result<Self> {
        db.busy_timeout(Duration::from_secs(5)).map_err(error)?;
        db.execute_batch("PRAGMA foreign_keys=ON; PRAGMA synchronous=FULL;
            CREATE TABLE IF NOT EXISTS plans(id TEXT PRIMARY KEY, body TEXT NOT NULL, digest TEXT NOT NULL, created REAL NOT NULL);
            CREATE TABLE IF NOT EXISTS tasks(plan TEXT NOT NULL,key TEXT NOT NULL,body TEXT NOT NULL,state TEXT NOT NULL,submitted REAL NOT NULL DEFAULT 0,PRIMARY KEY(plan,key),FOREIGN KEY(plan) REFERENCES plans(id));
            CREATE TABLE IF NOT EXISTS events(seq INTEGER PRIMARY KEY AUTOINCREMENT,plan TEXT NOT NULL,task TEXT NOT NULL,state TEXT NOT NULL,at REAL NOT NULL);").map_err(error)?;
        Ok(Self { db })
    }
    pub fn open(directory: &Path) -> Result<Self> {
        let root = directory.join("orchestration");
        crate::core::layouts::check_path(&root)?;
        if !root.exists() {
            #[cfg(unix)]
            {
                use std::os::unix::fs::DirBuilderExt;
                match fs::DirBuilder::new().mode(0o700).create(&root) {
                    Ok(()) => (),
                    Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => (),
                    Err(e) => return Err(error(e)),
                }
            }
            #[cfg(windows)]
            fs::create_dir(&root).map_err(error)?;
        }
        super::control_audit::directory(&root)?;
        let path = root.join("tasks.sqlite3");
        crate::core::layouts::check_path(&path)?;
        let mut options = fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
        }
        match options.open(&path) {
            Ok(_) => (),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => (),
            Err(e) => return Err(error(e)),
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            let m = fs::symlink_metadata(&path).map_err(error)?;
            if !m.is_file()
                || m.uid() != unsafe { libc::geteuid() }
                || m.mode() & 0o077 != 0
                || m.nlink() != 1
            {
                return Err("Unsafe orchestration database".into());
            }
        }
        Self::init(
            Connection::open_with_flags(
                path,
                rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE
                    | rusqlite::OpenFlags::SQLITE_OPEN_NOFOLLOW,
            )
            .map_err(error)?,
        )
    }
    pub fn admit(&mut self, plan: &Plan, approval: &str) -> Result<bool> {
        let digest = plan.fingerprint()?;
        if approval != digest {
            return Err("Orchestration review changed".into());
        }
        let tx = self
            .db
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .map_err(error)?;
        let old: Option<String> = tx
            .query_row("SELECT digest FROM plans WHERE id=?", [&plan.id], |r| {
                r.get(0)
            })
            .optional()
            .map_err(error)?;
        if let Some(old) = old {
            return if old == digest {
                Ok(false)
            } else {
                Err("Plan ID was reused with different tasks".into())
            };
        }
        let count: i64 = tx
            .query_row("SELECT COUNT(*) FROM plans", [], |r| r.get(0))
            .map_err(error)?;
        if count >= 1000 {
            return Err("Orchestration history is full; archive it before adding plans".into());
        }
        let at = now();
        tx.execute(
            "INSERT INTO plans VALUES(?,?,?,?)",
            params![
                plan.id,
                serde_json::to_string(plan).map_err(error)?,
                digest,
                at
            ],
        )
        .map_err(error)?;
        for task in &plan.tasks {
            tx.execute(
                "INSERT INTO tasks(plan,key,body,state) VALUES(?,?,?,'queued')",
                params![
                    plan.id,
                    task.key,
                    serde_json::to_string(task).map_err(error)?
                ],
            )
            .map_err(error)?;
            tx.execute(
                "INSERT INTO events(plan,task,state,at) VALUES(?,?,'queued',?)",
                params![plan.id, task.key, at],
            )
            .map_err(error)?;
        }
        tx.commit().map_err(error)?;
        Ok(true)
    }
    pub fn rows(&self) -> Result<Vec<Row>> {
        let mut stmt = self
            .db
            .prepare("SELECT plan,body,state,submitted FROM tasks ORDER BY rowid DESC LIMIT 32000")
            .map_err(error)?;
        let values = stmt
            .query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, f64>(3)?,
                ))
            })
            .map_err(error)?;
        values
            .map(|v| {
                let (plan, body, state, submitted) = v.map_err(error)?;
                Ok(Row {
                    plan,
                    task: serde_json::from_str(&body).map_err(error)?,
                    state,
                    submitted,
                })
            })
            .collect()
    }
    pub fn plans(&self) -> Result<Vec<Plan>> {
        let mut stmt = self
            .db
            .prepare("SELECT body FROM plans ORDER BY created DESC LIMIT 1000")
            .map_err(error)?;
        stmt.query_map([], |r| r.get::<_, String>(0))
            .map_err(error)?
            .map(|s| serde_json::from_str(&s.map_err(error)?).map_err(error))
            .collect()
    }
    fn transition(&mut self, row: &Row, state: &str, at: f64) -> Result<()> {
        let tx = self
            .db
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .map_err(error)?;
        let updated=tx.execute("UPDATE tasks SET state=?, submitted=CASE WHEN ?='submitted' THEN ? ELSE submitted END WHERE plan=? AND key=? AND state=?",
            params![state,state,at,row.plan,row.task.key,row.state]).map_err(error)?;
        if updated != 1 {
            return Err("Task changed; no action was replayed".into());
        }
        tx.execute(
            "INSERT INTO events(plan,task,state,at) VALUES(?,?,?,?)",
            params![row.plan, row.task.key, state, at],
        )
        .map_err(error)?;
        tx.commit().map_err(error)
    }
    pub fn cancel_queued(&mut self, plan: &str, key: &str) -> Result<()> {
        let row = self
            .rows()?
            .into_iter()
            .find(|r| r.plan == plan && r.task.key == key)
            .ok_or("Task disappeared")?;
        if row.state != "queued" {
            return Err("Only queued tasks can be cancelled here. Use the exact pane's Interrupt action for active work.".into());
        }
        self.transition(&row, "cancelled", now())
    }
    pub fn uncertain(&self, plan: &str, key: &str) -> Result<Row> {
        self.rows()?
            .into_iter()
            .find(|r| r.plan == plan && r.task.key == key && r.state == "unconfirmed")
            .ok_or("Only an uncertain task can be reviewed".into())
    }
    pub fn resolve(&mut self, plan: &str, key: &str) -> Result<()> {
        let row = self.uncertain(plan, key)?;
        self.transition(&row, "cancelled", now())
    }
    pub fn recover(&mut self) -> Result<()> {
        for row in self.rows()? {
            if active(&row.state) {
                self.transition(&row, "unconfirmed", now())?;
            }
        }
        Ok(())
    }
    pub fn events(&self) -> Result<Value> {
        let mut stmt = self
            .db
            .prepare("SELECT seq,plan,task,state,at FROM events ORDER BY seq DESC LIMIT 200")
            .map_err(error)?;
        let rows=stmt.query_map([],|r|Ok(serde_json::json!({"seq":r.get::<_,i64>(0)?,"plan":r.get::<_,String>(1)?,"task":r.get::<_,String>(2)?,"state":r.get::<_,String>(3)?,"at":r.get::<_,f64>(4)?}))).map_err(error)?;
        Ok(Value::Array(
            rows.collect::<rusqlite::Result<Vec<_>>>().map_err(error)?,
        ))
    }
}
fn active(state: &str) -> bool {
    matches!(
        state,
        "dispatching" | "submitted" | "running" | "waiting_approval"
    )
}
pub trait Executor {
    fn inspect(&self, task: &Task) -> Result<Value>;
    fn submit(&self, task: &Task) -> Result<Receipt>;
}
impl Executor for Terminals {
    fn inspect(&self, task: &Task) -> Result<Value> {
        self.verify(&task.pane)
    }
    fn submit(&self, task: &Task) -> Result<Receipt> {
        let actual = self.verify(&task.pane)?;
        if !same(task, &actual) || !matches!(state(&actual, now(), 0.0), "idle" | "done") {
            return Err("Agent identity or readiness changed before submission".into());
        }
        self.action(&task.pane, Action::Send(task.prompt.clone()))
    }
}
pub struct NativeExecutor<'a> {
    pub terminals: &'a Terminals,
    pub directory: &'a Path,
}
impl NativeExecutor<'_> {
    fn registration(&self, task: &Task) -> Result<Option<super::hook_runtime::Registration>> {
        let entries = match fs::read_dir(self.directory.join("hooks")) {
            Ok(entries) => entries,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(_) => return Err("Cannot inspect orchestration hook bindings".into()),
        };
        let mut found = None;
        for (i, entry) in entries.enumerate() {
            if i >= 4096 {
                return Err("Too many hook registrations".into());
            }
            let path = entry.map_err(error)?.path();
            if path.extension().is_none_or(|s| s != "json") {
                continue;
            }
            let r = super::hook_runtime::inspect(&path).map_err(error)?;
            if r.directory != self.directory || r.path() != path {
                return Err("Hook belongs to another instance".into());
            }
            if r.scope.terminal != task.identity || r.harness_process.is_none() {
                continue;
            }
            if super::hook_runtime::verify_live_session(&r, self.terminals).is_err() {
                continue;
            }
            if found.is_some() {
                return Err("Multiple live hook owners for the selected pane".into());
            }
            if task.session.as_deref() != Some(r.scope.conversation.as_str())
                || serde_json::to_value(r.scope.harness)
                    .map_err(error)?
                    .as_str()
                    != Some(task.agent.as_str())
            {
                return Err("The registered provider or conversation changed".into());
            }
            found = Some(r);
        }
        Ok(found)
    }
    pub fn validate_admission(&self, task: &Task) -> Result<()> {
        let actual = self.inspect(task)?;
        if !same(task, &actual) {
            return Err("An agent or conversation changed; review the plan again".into());
        }
        if !matches!(
            state(&actual, now(), 0.0),
            "working" | "blocked" | "idle" | "done"
        ) {
            return Err("This pane has no verified lifecycle state. Install its native hooks before scheduling tasks.".into());
        }
        Ok(())
    }
    pub fn validate_release(&self, task: &Task) -> Result<()> {
        let actual = self.inspect(task)?;
        if !same(task, &actual) || !matches!(state(&actual, now(), 0.0), "idle" | "done") {
            return Err(
                "Inspect the original agent and wait for verified idle before releasing its pane"
                    .into(),
            );
        }
        Ok(())
    }
}
impl Executor for NativeExecutor<'_> {
    fn inspect(&self, task: &Task) -> Result<Value> {
        let actual = self.terminals.verify(&task.pane)?;
        let mut snapshot = serde_json::json!({"panes":[actual]});
        super::session_actions::enrich_sessions(self.directory, &mut snapshot, self.terminals)?;
        let mut actual = snapshot["panes"][0].clone();
        if let Some(r) = self.registration(task)? {
            let ready = super::hook_runtime::ready_snapshot(&r.path(), &self.terminals.proc_root)
                .map_err(error)?;
            if ready.scope != r.scope {
                return Err("Readiness belongs to another session".into());
            }
            actual["orchestration_readiness"] = serde_json::to_value(ready.state).map_err(error)?;
        } else if task.identity.backend != crate::core::identity::Backend::Herdr {
            return Err(
                "Native and tmux orchestration requires an exact live hook registration".into(),
            );
        }
        Ok(actual)
    }
    fn submit(&self, task: &Task) -> Result<Receipt> {
        let actual = self.inspect(task)?;
        if !same(task, &actual) || !matches!(state(&actual, now(), 0.0), "idle" | "done") {
            return Err("Agent identity or readiness changed before submission".into());
        }
        if let Some(r) = self.registration(task)? {
            let ready = super::hook_runtime::ready_snapshot(&r.path(), &self.terminals.proc_root)
                .map_err(error)?;
            if ready.scope != r.scope
                || !super::hook_runtime::reserve_ready_for(
                    &r.path(),
                    &self.terminals.proc_root,
                    ready.sequence,
                    &task.prompt,
                )
                .map_err(error)?
            {
                return Err("Native readiness was consumed by another controller".into());
            }
            super::hook_runtime::verify_live_session(&r, self.terminals).map_err(error)?;
        }
        self.terminals
            .action(&actual, Action::Send(task.prompt.clone()))
    }
}
fn state(actual: &Value, at: f64, after: f64) -> &str {
    if let Ok(observed) = serde_json::from_value::<super::agent_activity::Observation>(
        actual["agent_activity"].clone(),
    ) && observed.fresh(at)
        && observed.observed_at > after
    {
        return actual["agent_activity"]["state"]
            .as_str()
            .unwrap_or("unknown");
    }
    if after == 0.0
        && let Some(ready) = actual["orchestration_readiness"].as_str()
    {
        return match ready {
            "idle" => "idle",
            "busy" => "working",
            _ => "unknown",
        };
    }
    actual["agent_status"].as_str().unwrap_or("unknown")
}
fn same(task: &Task, actual: &Value) -> bool {
    terminals::identity(actual).is_ok_and(|id| id == task.identity)
        && actual["agent"].as_str() == Some(task.agent.as_str())
        && task
            .session
            .as_deref()
            .is_none_or(|id| terminals::reported_session(actual) == Some(id))
}
/// Called with the instance's exclusive worker lock. Side effects occur only
/// after their dispatching event commits; any doubtful send stays unconfirmed.
pub fn tick(store: &mut Store, executor: &impl Executor, at: f64) -> Result<bool> {
    for row in store.rows()?.into_iter().filter(|r| active(&r.state)) {
        let actual = match executor.inspect(&row.task) {
            Ok(actual) if same(&row.task, &actual) => actual,
            _ => {
                store.transition(&row, "unconfirmed", at)?;
                continue;
            }
        };
        let status = state(&actual, at, row.submitted);
        let next = match (row.state.as_str(), status) {
            (_, "working") => Some("running"),
            (_, "blocked") => Some("waiting_approval"),
            ("running" | "waiting_approval", "idle" | "done") => Some("completed"),
            ("submitted", _) if at - row.submitted > 15.0 => Some("unconfirmed"),
            _ => None,
        };
        if let Some(next) = next.filter(|next| *next != row.state) {
            store.transition(&row, next, at)?;
        }
    }
    let plans = store.plans()?;
    for plan in plans {
        let mut rows = store.rows()?;
        rows.reverse();
        let mut count = rows
            .iter()
            .filter(|r| r.plan == plan.id && active(&r.state))
            .count();
        let mut global_count = rows.iter().filter(|r| active(&r.state)).count();
        let mut occupied = rows
            .iter()
            .filter(|r| active(&r.state) || r.state == "unconfirmed")
            .map(|r| r.task.identity.clone())
            .collect::<Vec<_>>();
        for row in rows
            .iter()
            .filter(|r| r.plan == plan.id && r.state == "queued")
        {
            let deps = rows
                .iter()
                .filter(|r| r.plan == row.plan && row.task.dependencies.contains(&r.task.key))
                .collect::<Vec<_>>();
            if deps.iter().any(|r| {
                matches!(
                    r.state.as_str(),
                    "unconfirmed" | "failed" | "cancelled" | "skipped"
                )
            }) {
                store.transition(row, "skipped", at)?;
                continue;
            }
            if deps.iter().any(|r| r.state != "completed")
                || count >= plan.parallel
                || global_count >= 32
                || occupied.contains(&row.task.identity)
            {
                continue;
            }
            let actual = match executor.inspect(&row.task) {
                Ok(actual) if same(&row.task, &actual) => actual,
                _ => {
                    store.transition(row, "unconfirmed", at)?;
                    continue;
                }
            };
            if !matches!(state(&actual, at, 0.0), "idle" | "done") {
                continue;
            }
            if let Err(error) = store.transition(row, "dispatching", at) {
                if store.rows()?.iter().any(|current| {
                    current.plan == row.plan
                        && current.task.key == row.task.key
                        && current.state != "queued"
                }) {
                    continue;
                }
                return Err(error);
            }
            let dispatched = Row {
                state: "dispatching".into(),
                ..row.clone()
            };
            let next = match executor.submit(&row.task) {
                Ok(receipt) if receipt.state != ReceiptState::Unconfirmed => "submitted",
                _ => "unconfirmed",
            };
            store.transition(&dispatched, next, at)?;
            occupied.push(row.task.identity.clone());
            if next == "submitted" {
                count += 1;
                global_count += 1;
            }
        }
    }
    Ok(store
        .rows()?
        .iter()
        .any(|r| r.state == "queued" || active(&r.state)))
}

pub fn spawn_worker(directory: &Path) -> Result<()> {
    let mut command = std::process::Command::new(std::env::current_exe().map_err(error)?);
    command
        .args(["orchestrate", "worker", "--config-dir"])
        .arg(directory)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        unsafe {
            command.pre_exec(|| {
                if libc::setsid() < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
    }
    crate::platform::hide_console(&mut command);
    let mut child=command.spawn().map_err(|_|"Tasks were saved, but the worker could not start. Inspect the plan; no task was replayed.".to_string())?;
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    Ok(())
}
pub fn run(args: &[String], out: &mut impl std::io::Write) -> Result<()> {
    if args.is_empty() || args == ["--help"] {
        return writeln!(out,"daedalus orchestrate list|events|worker --config-dir ABS\nPlans are reviewed in the TUI (F7). Workers persist intent before sending, require observed native work before completion, and never replay uncertain sends.").map_err(error);
    }
    if args.len() != 3 || args[1] != "--config-dir" || !Path::new(&args[2]).is_absolute() {
        return Err("Use orchestrate list|events|worker --config-dir ABS".into());
    }
    if !matches!(args[0].as_str(), "list" | "events" | "worker") {
        return Err("Unknown orchestration command".into());
    }
    let directory = PathBuf::from(&args[2]);
    crate::core::layouts::check_path(&directory)?;
    if args[0] != "worker" && !directory.join("orchestration/tasks.sqlite3").exists() {
        return writeln!(out, "[]").map_err(error);
    }
    let mut store = Store::open(&directory)?;
    match args[0].as_str() {
        "list" => writeln!(
            out,
            "{}",
            serde_json::to_string(&store.rows()?).map_err(error)?
        )
        .map_err(error),
        "events" => writeln!(out, "{}", store.events()?).map_err(error),
        "worker" => {
            let _lock = super::control_audit::lock(&directory.join("orchestration"))?;
            let bytes = crate::core::layouts::read_private(&directory.join("control.json"))?
                .ok_or("Configure this instance before running orchestration")?;
            let config = super::config::ControlConfig::parse(&bytes)?;
            let mut executor = Terminals::default();
            executor.apply_settings(&config.runtime)?;
            store.recover()?;
            let native = NativeExecutor {
                terminals: &executor,
                directory: &directory,
            };
            loop {
                let outstanding = tick(&mut store, &native, now())?;
                std::thread::sleep(Duration::from_millis(if outstanding { 500 } else { 2000 }));
            }
        }
        _ => Err("Unknown orchestration command".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::cell::RefCell;
    struct Fake {
        panes: RefCell<Vec<Value>>,
        sent: RefCell<Vec<String>>,
        fail: bool,
    }
    fn task(key: &str, pane: &str, deps: &[&str]) -> Task {
        let pane = json!({"backend":"herdr","session":"fixture","server":"fixture","pane_id":pane,"terminal_id":"generation","agent":"codex","agent_status":"idle","agent_session":{"kind":"id","agent":"codex","value":"fixture-conversation"}});
        Task {
            key: key.into(),
            label: key.into(),
            identity: terminals::identity(&pane).unwrap(),
            agent: "codex".into(),
            session: Some("fixture-conversation".into()),
            pane,
            prompt: format!("Please execute task {key}"),
            dependencies: deps.iter().map(|s| s.to_string()).collect(),
        }
    }
    fn plan(tasks: Vec<Task>, parallel: usize) -> Plan {
        Plan {
            id: "a-123".into(),
            name: "Fixture plan".into(),
            parallel,
            tasks,
        }
    }
    fn save(plan: &Plan) -> Store {
        let mut s = Store::memory().unwrap();
        s.admit(plan, &plan.fingerprint().unwrap()).unwrap();
        s
    }
    fn fake(plan: &Plan) -> Fake {
        Fake {
            panes: RefCell::new(plan.tasks.iter().map(|t| t.pane.clone()).collect()),
            sent: RefCell::new(Vec::new()),
            fail: false,
        }
    }
    impl Fake {
        fn status(&self, pane: &str, status: &str) {
            for p in self
                .panes
                .borrow_mut()
                .iter_mut()
                .filter(|p| p["pane_id"] == pane)
            {
                p["agent_status"] = json!(status);
            }
        }
    }
    impl Executor for Fake {
        fn inspect(&self, task: &Task) -> Result<Value> {
            self.panes
                .borrow()
                .iter()
                .find(|p| p["pane_id"] == task.identity.pane)
                .cloned()
                .ok_or("Missing pane".into())
        }
        fn submit(&self, task: &Task) -> Result<Receipt> {
            self.sent.borrow_mut().push(task.key.clone());
            if self.fail {
                return Err("Lost acknowledgement after send".into());
            }
            Ok(Receipt {
                state: ReceiptState::Submitted,
                message: "fixture".into(),
                terminal: None,
            })
        }
    }
    fn status(s: &Store, key: &str) -> String {
        s.rows()
            .unwrap()
            .into_iter()
            .find(|r| r.task.key == key)
            .unwrap()
            .state
    }
    #[test]
    fn admission_is_atomic_idempotent_and_rejects_cycles_or_changed_approval() {
        let mut p = plan(vec![task("one", "p1", &[]), task("two", "p2", &["one"])], 2);
        let mut s = save(&p);
        assert!(!s.admit(&p, &p.fingerprint().unwrap()).unwrap());
        assert_eq!(s.rows().unwrap().len(), 2);
        p.tasks[0].prompt.push('!');
        assert!(s.admit(&p, &p.fingerprint().unwrap()).is_err());
        p.id = "b-123".into();
        assert!(s.admit(&p, "stale approval").is_err());
        p.tasks[0].dependencies = vec!["two".into()];
        assert!(p.validate().is_err());
        assert_eq!(s.events().unwrap().as_array().unwrap().len(), 2);
    }
    #[test]
    fn dependencies_wait_for_observed_work_and_turn_end() {
        let p = plan(vec![task("one", "p1", &[]), task("two", "p2", &["one"])], 2);
        let mut s = save(&p);
        let e = fake(&p);
        tick(&mut s, &e, 100.0).unwrap();
        assert_eq!(*e.sent.borrow(), ["one"]);
        assert_eq!(status(&s, "one"), "submitted");
        tick(&mut s, &e, 101.0).unwrap();
        assert_eq!(status(&s, "one"), "submitted");
        e.status("p1", "working");
        tick(&mut s, &e, 102.0).unwrap();
        assert_eq!(status(&s, "one"), "running");
        e.status("p1", "blocked");
        tick(&mut s, &e, 103.0).unwrap();
        assert_eq!(status(&s, "one"), "waiting_approval");
        assert_eq!(e.sent.borrow().len(), 1);
        e.status("p1", "idle");
        tick(&mut s, &e, 104.0).unwrap();
        assert_eq!(status(&s, "one"), "completed");
        assert_eq!(*e.sent.borrow(), ["one", "two"]);
    }
    #[test]
    fn busy_agents_parallel_limit_and_shared_pane_exclusion_are_enforced() {
        let p = plan(
            vec![
                task("one", "p1", &[]),
                task("two", "p2", &[]),
                task("three", "p1", &[]),
            ],
            1,
        );
        let mut s = save(&p);
        let e = fake(&p);
        e.status("p1", "working");
        tick(&mut s, &e, 100.0).unwrap();
        assert_eq!(*e.sent.borrow(), ["two"]);
        e.status("p1", "idle");
        tick(&mut s, &e, 101.0).unwrap();
        assert_eq!(e.sent.borrow().len(), 1);
        e.status("p2", "working");
        tick(&mut s, &e, 102.0).unwrap();
        e.status("p2", "done");
        tick(&mut s, &e, 103.0).unwrap();
        assert_eq!(*e.sent.borrow(), ["two", "one"]);
        let mut p2 = plan(vec![task("four", "p1", &[])], 2);
        p2.id = "b-123".into();
        s.admit(&p2, &p2.fingerprint().unwrap()).unwrap();
        tick(&mut s, &e, 104.0).unwrap();
        assert_eq!(e.sent.borrow().len(), 2);
    }
    #[test]
    fn uncertain_send_never_replays_and_reserves_its_pane_until_review() {
        let p = plan(
            vec![
                task("one", "p1", &[]),
                task("two", "p2", &["one"]),
                task("three", "p1", &[]),
            ],
            2,
        );
        let mut s = save(&p);
        let mut e = fake(&p);
        e.fail = true;
        tick(&mut s, &e, 100.0).unwrap();
        tick(&mut s, &e, 101.0).unwrap();
        assert_eq!(status(&s, "one"), "unconfirmed");
        assert_eq!(status(&s, "two"), "skipped");
        assert_eq!(status(&s, "three"), "queued");
        assert_eq!(*e.sent.borrow(), ["one"]);
        s.resolve(&p.id, "one").unwrap();
        e.fail = false;
        tick(&mut s, &e, 102.0).unwrap();
        assert_eq!(*e.sent.borrow(), ["one", "three"]);
    }
    #[test]
    fn crash_recovery_quarantines_dispatch_and_submitted_tasks() {
        let p = plan(vec![task("one", "p1", &[]), task("two", "p2", &[])], 2);
        let mut s = save(&p);
        let e = fake(&p);
        let row = s
            .rows()
            .unwrap()
            .into_iter()
            .find(|r| r.task.key == "one")
            .unwrap();
        s.transition(&row, "dispatching", 100.0).unwrap();
        tick(&mut s, &e, 100.0).unwrap();
        s.recover().unwrap();
        tick(&mut s, &e, 110.0).unwrap();
        assert!(s.rows().unwrap().iter().all(|r| r.state == "unconfirmed"));
        assert_eq!(*e.sent.borrow(), ["two"]);
    }
    #[test]
    fn changed_generation_or_conversation_is_never_substituted() {
        for field in ["terminal_id", "conversation"] {
            let p = plan(vec![task("one", "p1", &[])], 1);
            let mut s = save(&p);
            let e = fake(&p);
            if field == "conversation" {
                e.panes.borrow_mut()[0]["agent_session"]["value"] = json!("replacement");
            } else {
                e.panes.borrow_mut()[0][field] = json!("replacement");
            }
            tick(&mut s, &e, 100.0).unwrap();
            assert!(e.sent.borrow().is_empty());
            assert_eq!(status(&s, "one"), "unconfirmed");
        }
    }
    #[test]
    fn idle_without_observed_work_is_uncertain_and_cancel_is_single_use() {
        let p = plan(vec![task("one", "p1", &[]), task("two", "p2", &["one"])], 1);
        let mut s = save(&p);
        let e = fake(&p);
        s.cancel_queued(&p.id, "two").unwrap();
        assert!(s.cancel_queued(&p.id, "two").is_err());
        tick(&mut s, &e, 100.0).unwrap();
        tick(&mut s, &e, 116.0).unwrap();
        assert_eq!(status(&s, "one"), "unconfirmed");
        assert_eq!(*e.sent.borrow(), ["one"]);
    }
    #[test]
    fn readiness_reservation_does_not_count_as_observed_model_activity() {
        let actual = json!({"orchestration_readiness":"busy","agent_activity":{"state":"working","observed_at":90.0,"expires_at":200.0}});
        assert_eq!(state(&actual, 101.0, 100.0), "unknown");
        assert_eq!(state(&actual, 101.0, 0.0), "working");
        let mut real = actual;
        real["agent_activity"]["observed_at"] = json!(100.5);
        assert_eq!(state(&real, 101.0, 100.0), "working");
    }
    #[test]
    fn empty_inspection_and_invalid_commands_do_not_create_queue_state() {
        let root = std::env::temp_dir().join(format!("daedalus-empty-queue-{}", new_id()));
        for operation in ["list", "events"] {
            let mut output = Vec::new();
            run(
                &[
                    operation.into(),
                    "--config-dir".into(),
                    root.to_string_lossy().into_owned(),
                ],
                &mut output,
            )
            .unwrap();
            assert_eq!(output, b"[]\n");
            assert!(!root.exists());
        }
        assert!(
            run(
                &[
                    "invalid".into(),
                    "--config-dir".into(),
                    root.to_string_lossy().into_owned()
                ],
                &mut Vec::new()
            )
            .is_err()
        );
        assert!(!root.exists());
    }
}
