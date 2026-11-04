//! Local aggregate observations of Herdr's server selection, not human attention.
//! Only hashed identities, counts and timestamps are persisted; never pane text.
use crate::{
    adapters::terminals,
    core::{
        identity::TerminalId,
        usage_budget::{Guard, Policy},
    },
};
use rusqlite::{Connection, OptionalExtension, params};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    path::Path,
    sync::OnceLock,
    time::{SystemTime, UNIX_EPOCH},
};
type Result<T> = std::result::Result<T, String>;
fn hash(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
pub fn key(pane: &Value) -> Option<String> {
    Some(hash(
        &serde_json::to_vec(&terminals::identity(pane).ok()?).ok()?,
    ))
}
fn run() -> &'static str {
    static RUN: OnceLock<String> = OnceLock::new();
    RUN.get_or_init(|| {
        format!(
            "{}:{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        )
    })
}

const SCHEMA: &str = "CREATE TABLE IF NOT EXISTS workspace_usage(terminal TEXT PRIMARY KEY,visits INTEGER NOT NULL,seconds REAL NOT NULL,last_selected REAL NOT NULL);
CREATE TABLE IF NOT EXISTS workspace_focus(channel TEXT PRIMARY KEY,terminal TEXT NOT NULL,observed REAL NOT NULL,owner TEXT NOT NULL);";
pub(crate) struct Prepared {
    pub live: BTreeMap<String, TerminalId>,
    selections: BTreeMap<String, String>,
}
pub(crate) fn prepare(snapshot: &Value, guard: &mut Guard, focus: bool) -> Result<Prepared> {
    let mut live = BTreeMap::new();
    let mut index = BTreeMap::<(&str, &str), Option<String>>::new();
    for pane in snapshot["panes"].as_array().into_iter().flatten() {
        guard.entries(1)?;
        if pane["backend"] != "herdr" {
            continue;
        }
        // Admit only routing metadata; pane contents/titles are never serialized.
        for field in ["backend", "session", "pane_id", "terminal_id"] {
            guard.bytes(pane[field].as_str().map_or(0, str::len))?;
        }
        let Ok(identity) = terminals::identity(pane) else {
            continue;
        };
        let encoded = guard.encode(&identity)?;
        guard.bytes(64)?;
        let key = hash(&encoded);
        let session = pane["session"].as_str().unwrap_or_default();
        let pane_id = pane["pane_id"].as_str().unwrap_or_default();
        index
            .entry((session, pane_id))
            .and_modify(|previous| *previous = None)
            .or_insert(Some(key.clone()));
        live.insert(key, identity);
    }
    let mut selections = BTreeMap::<String, Option<String>>::new();
    if focus {
        for row in snapshot["herdr_focus"].as_array().into_iter().flatten() {
            guard.entries(1)?;
            let (Some(session), Some(pane)) = (
                row["session"].as_str(),
                row["pane_id"].as_str().filter(|s| !s.is_empty()),
            ) else {
                continue;
            };
            guard.bytes(session.len().saturating_add(pane.len()))?;
            let Some(Some(key)) = index.get(&(session, pane)) else {
                continue;
            };
            guard.bytes(128)?;
            selections
                .entry(hash(session.as_bytes()))
                .and_modify(|previous| {
                    if previous.as_ref() != Some(key) {
                        *previous = None;
                    }
                })
                .or_insert(Some(key.clone()));
        }
    }
    guard.check()?;
    Ok(Prepared {
        live,
        selections: selections
            .into_iter()
            .filter_map(|(channel, key)| Some((channel, key?)))
            .collect(),
    })
}
fn hash_key(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|b| b.is_ascii_hexdigit())
}
fn text(row: &rusqlite::Row<'_>, index: usize, guard: &mut Guard) -> Result<String> {
    let raw = guard
        .sql(row.get_ref(index))?
        .as_str()
        .map_err(|_| "Invalid workspace selection lease")?;
    guard.bytes(raw.len())?;
    Ok(raw.to_owned())
}
pub(crate) fn aggregates(
    con: &Connection,
    live: &BTreeMap<String, TerminalId>,
    guard: &mut Guard,
) -> Result<Value> {
    let mut query = guard.sql(
        con.prepare("SELECT visits,seconds,last_selected FROM workspace_usage WHERE terminal=?"),
    )?;
    let mut result = serde_json::Map::new();
    for key in live.keys() {
        guard.entries(1)?;
        let row = guard.sql(
            query
                .query_row([key], |r| {
                    Ok((
                        r.get::<_, i64>(0)?,
                        r.get::<_, f64>(1)?,
                        r.get::<_, f64>(2)?,
                    ))
                })
                .optional(),
        )?;
        if let Some((visits, seconds, last)) = row {
            if visits < 0
                || !seconds.is_finite()
                || seconds < 0.0
                || !last.is_finite()
                || last < 0.0
            {
                return Err("Invalid workspace usage aggregate".into());
            }
            guard.bytes(key.len() + 24)?;
            result.insert(
                key.clone(),
                json!({"visits":visits,"seconds":seconds,"last_selected":last}),
            );
        }
    }
    guard.check()?;
    Ok(Value::Object(result))
}
fn observe_with(
    con: &mut Connection,
    prepared: &Prepared,
    now: f64,
    owner: &str,
    accrue: bool,
    guard: &mut Guard,
) -> Result<Value> {
    if !now.is_finite() || now < 0.0 {
        return Err("Invalid usage timestamp".into());
    }
    guard.check()?;
    let tx = guard.sql(con.transaction())?;
    let result = (|| {
        guard.database(&tx, true)?;
        guard.sql(tx.execute_batch("PRAGMA trusted_schema=OFF;"))?;
        guard.sql(tx.execute_batch(SCHEMA))?;
        let mut previous = BTreeMap::new();
        {
            let mut query = guard
                .sql(tx.prepare("SELECT channel,terminal,observed,owner FROM workspace_focus"))?;
            let mut rows = guard.sql(query.query([]))?;
            while let Some(row) = guard.sql(rows.next())? {
                guard.entries(1)?;
                let channel = text(row, 0, guard)?;
                let terminal = text(row, 1, guard)?;
                let at: f64 = guard.sql(row.get(2))?;
                let run = text(row, 3, guard)?;
                if !hash_key(&channel)
                    || !hash_key(&terminal)
                    || !at.is_finite()
                    || at < 0.0
                    || run.len() > 128
                    || run.is_empty()
                {
                    return Err("Invalid workspace selection lease".into());
                }
                previous.insert(channel, (terminal, at, run));
            }
        }
        guard.sql(tx.execute("DELETE FROM workspace_focus WHERE owner=?", [owner]))?;
        let lease = guard.policy.lease_ms as f64 / 1000.0;
        let max_gap = guard.policy.max_gap_ms as f64 / 1000.0;
        for (channel, terminal) in &prepared.selections {
            guard.entries(1)?;
            let old = previous.get(channel);
            if old.is_some_and(|(_, at, run)| run != owner && now - *at <= lease) {
                continue;
            }
            let same = old.is_some_and(|(id, _, run)| id == terminal && run == owner);
            let changed = old.is_none_or(|(id, _, _)| id != terminal);
            let elapsed = old
                .filter(|(_, at, _)| accrue && same && now >= *at && now - *at <= max_gap)
                .map_or(0.0, |(_, at, _)| now - at);
            guard.sql(tx.execute("INSERT INTO workspace_usage VALUES(?1,?2,?3,?4) ON CONFLICT(terminal) DO UPDATE SET visits=visits+excluded.visits,seconds=seconds+excluded.seconds,last_selected=MAX(last_selected,excluded.last_selected)",params![terminal,i64::from(changed),elapsed,now]))?;
            guard.sql(tx.execute(
                "INSERT OR REPLACE INTO workspace_focus VALUES(?,?,?,?)",
                params![
                    channel,
                    terminal,
                    old.map_or(now, |(_, at, _)| now.max(*at)),
                    owner
                ],
            ))?;
        }
        guard.sql(tx.execute(
            "DELETE FROM workspace_focus WHERE observed<?",
            [now - lease],
        ))?;
        if guard.policy.retention_days > 0 {
            let cutoff = now - guard.policy.retention_days as f64 * 86400.0;
            guard.sql(tx.execute("DELETE FROM workspace_usage WHERE typeof(last_selected) IN ('real','integer') AND last_selected>=0 AND last_selected<?",[cutoff]))?;
        }
        let result = aggregates(&tx, &prepared.live, guard)?;
        guard.encode(&result)?;
        guard.check()?;
        Ok(result)
    })();
    // Remove the interrupt hook before rollback on any refusal. A successful
    // COMMIT is reported as complete even if its blocking fsync ran past time.
    let result = match result {
        Ok(value) => match tx.execute_batch("COMMIT") {
            Ok(()) => Ok(value),
            Err(error) => guard.sql(Err(error)),
        },
        Err(error) => Err(error),
    };
    Guard::unhook(&tx);
    drop(tx);
    result
}
pub struct Recorder {
    enabled_last_sample: bool,
    owner: String,
}
impl Default for Recorder {
    fn default() -> Self {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        Self {
            enabled_last_sample: false,
            owner: format!(
                "{}:{}",
                run(),
                NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            ),
        }
    }
}
impl Recorder {
    pub fn sample(
        &mut self,
        directory: &Path,
        snapshot: &Value,
        now: f64,
        enabled: bool,
        policy: Policy,
        deadline: Option<std::time::Instant>,
    ) -> Result<Option<Value>> {
        let accrue = self.enabled_last_sample;
        self.enabled_last_sample = false;
        if !enabled {
            return Ok(None);
        }
        if !now.is_finite() || now < 0.0 {
            return Err("Invalid usage timestamp".into());
        }
        let mut guard = Guard::new(policy, deadline)?;
        let prepared = prepare(snapshot, &mut guard, true)?;
        let path = directory.join("workspace-usage.sqlite3");
        guard.database_files(&path)?;
        let mut con =
            super::telegram_config::private_connection_with(&path, |con| guard.configure(con))
                .map_err(|error| guard.check().err().unwrap_or(error))?;
        let result = observe_with(&mut con, &prepared, now, &self.owner, accrue, &mut guard);
        Guard::unhook(&con);
        let value = result?;
        self.enabled_last_sample = true;
        Ok(Some(value))
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    fn open(con: Connection) -> Result<Connection> {
        con.execute_batch(SCHEMA).map_err(|_| "fixture schema")?;
        Ok(con)
    }
    fn observe(con: &mut Connection, snapshot: &Value, now: f64, owner: &str) -> Result<Value> {
        let mut guard = Guard::new(Policy::default(), None)?;
        let prepared = prepare(snapshot, &mut guard, true)?;
        guard.configure(con)?;
        observe_with(con, &prepared, now, owner, true, &mut guard)
    }
    fn selected(snapshot: &Value) -> BTreeMap<String, String> {
        prepare(
            snapshot,
            &mut Guard::new(Policy::default(), None).unwrap(),
            true,
        )
        .unwrap()
        .selections
    }

    struct Fixture(std::path::PathBuf);
    impl Fixture {
        fn new() -> Self {
            let mut nonce = [0; 16];
            crate::platform::random_bytes(&mut nonce).unwrap();
            let path = std::env::temp_dir()
                .join(format!("dd-usage-policy-{:x}", u128::from_le_bytes(nonce)));
            crate::core::layouts::write_private(&path.join("marker"), b"").unwrap();
            Self(path)
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    #[test]
    fn disabled_recording_never_opens_state_and_resume_excludes_the_disabled_interval() {
        let f = Fixture::new();
        let mut recorder = Recorder::default();
        let state = f.0.join("absent");
        let data = snapshot("first");
        let key = key(&data["panes"][0]).unwrap();
        assert!(
            recorder
                .sample(&state, &data, 100.0, false, Policy::default(), None)
                .unwrap()
                .is_none()
        );
        assert!(!state.exists());
        let first = recorder
            .sample(&f.0, &data, 100.0, true, Policy::default(), None)
            .unwrap()
            .unwrap();
        assert_eq!(first[&key]["seconds"], 0.0);
        let next = recorder
            .sample(&f.0, &data, 105.0, true, Policy::default(), None)
            .unwrap()
            .unwrap();
        assert_eq!(next[&key]["seconds"], 5.0);
        let path = f.0.join("workspace-usage.sqlite3");
        let before = std::fs::read(&path).unwrap();
        assert!(
            recorder
                .sample(&f.0, &data, 106.0, false, Policy::default(), None)
                .unwrap()
                .is_none()
        );
        assert_eq!(std::fs::read(&path).unwrap(), before);
        let resumed = recorder
            .sample(&f.0, &data, 110.0, true, Policy::default(), None)
            .unwrap()
            .unwrap();
        assert_eq!(resumed[&key]["seconds"], 5.0);
        assert_eq!(resumed[&key]["visits"], 1);
        let expired = std::time::Instant::now();
        assert!(
            recorder
                .sample(&f.0, &data, 111.0, true, Policy::default(), Some(expired))
                .unwrap_err()
                .contains("workspace_usage_timeout_ms")
        );
        let recovered = recorder
            .sample(&f.0, &data, 112.0, true, Policy::default(), None)
            .unwrap()
            .unwrap();
        assert_eq!(recovered[&key]["seconds"], 5.0);
    }
    #[test]
    fn configured_gap_and_lease_apply_without_same_process_double_counting() {
        let f = Fixture::new();
        let data = snapshot("first");
        let key = key(&data["panes"][0]).unwrap();
        let policy = Policy {
            max_gap_ms: 1000,
            lease_ms: 1000,
            ..Default::default()
        };
        let mut one = Recorder::default();
        let mut two = Recorder::default();
        one.sample(&f.0, &data, 100.0, true, policy, None).unwrap();
        two.sample(&f.0, &data, 100.5, true, policy, None).unwrap();
        let value = one
            .sample(&f.0, &data, 100.8, true, policy, None)
            .unwrap()
            .unwrap();
        assert!((value[&key]["seconds"].as_f64().unwrap() - 0.8).abs() < 0.00001);
        let value = two
            .sample(&f.0, &data, 105.0, true, policy, None)
            .unwrap()
            .unwrap();
        assert!((value[&key]["seconds"].as_f64().unwrap() - 0.8).abs() < 0.00001);
        let value = two
            .sample(&f.0, &data, 110.0, true, policy, None)
            .unwrap()
            .unwrap();
        assert!((value[&key]["seconds"].as_f64().unwrap() - 0.8).abs() < 0.00001);
    }
    #[test]
    fn retention_is_opt_in_and_only_expires_old_aggregate_rows() {
        let mut con = open(Connection::open_in_memory().unwrap()).unwrap();
        let old = snapshot("old");
        let current = snapshot("current");
        let oldkey = key(&old["panes"][0]).unwrap();
        observe(&mut con, &old, 100.0, "one").unwrap();
        observe(&mut con, &current, 200000.0, "one").unwrap();
        assert_eq!(
            con.query_row("SELECT COUNT(*) FROM workspace_usage", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            2
        );
        let mut guard = Guard::new(
            Policy {
                retention_days: 1,
                ..Default::default()
            },
            None,
        )
        .unwrap();
        let prepared = prepare(&current, &mut guard, true).unwrap();
        guard.configure(&con).unwrap();
        observe_with(&mut con, &prepared, 200001.0, "one", true, &mut guard).unwrap();
        assert_eq!(
            con.query_row(
                "SELECT COUNT(*) FROM workspace_usage WHERE terminal=?",
                [oldkey],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
            0
        );
        assert_eq!(
            con.query_row("SELECT COUNT(*) FROM workspace_usage", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            1
        );
    }
    #[test]
    fn sql_and_projection_refusals_roll_back_the_entire_sample() {
        let mut con = open(Connection::open_in_memory().unwrap()).unwrap();
        let panes:Vec<_>=(0..20).map(|i|json!({"backend":"herdr","session":format!("server-{i}"),"pane_id":"pane","terminal_id":"generation","content":"NEVER_READ_THIS_BODY"})).collect();
        let focus: Vec<_> = (0..20)
            .map(|i| json!({"session":format!("server-{i}"),"pane_id":"pane"}))
            .collect();
        let data = json!({"panes":panes,"herdr_focus":focus});
        for (policy, key) in [
            (
                Policy {
                    sql_steps: 500,
                    ..Default::default()
                },
                "workspace_usage_sql_step_limit",
            ),
            (
                Policy {
                    output_bytes: 1024,
                    ..Default::default()
                },
                "workspace_usage_output_limit_bytes",
            ),
        ] {
            let mut guard = Guard::new(policy, None).unwrap();
            let prepared = prepare(&data, &mut guard, true).unwrap();
            guard.configure(&con).unwrap();
            let changes = con.total_changes();
            assert!(
                observe_with(&mut con, &prepared, 100.0, "one", true, &mut guard)
                    .unwrap_err()
                    .contains(key)
            );
            assert!(
                con.total_changes() > changes,
                "Fixture must exercise rollback after actual row changes"
            );
            assert!(con.is_autocommit());
            for table in ["workspace_usage", "workspace_focus"] {
                assert_eq!(
                    con.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r
                        .get::<_, i64>(0))
                        .unwrap(),
                    0
                );
            }
        }
        let value = observe(&mut con, &data, 100.0, "one").unwrap();
        assert_eq!(value.as_object().unwrap().len(), 20);
        assert!(!value.to_string().contains("NEVER_READ_THIS_BODY"));
    }
    #[test]
    fn ignored_entries_and_oversized_routing_metadata_stop_before_state_creation() {
        let f = Fixture::new();
        let mut recorder = Recorder::default();
        let mut data = snapshot("first");
        data["panes"]
            .as_array_mut()
            .unwrap()
            .extend([json!({"backend":"ignored"}), json!({"backend":"ignored"})]);
        assert!(
            recorder
                .sample(
                    &f.0,
                    &data,
                    100.0,
                    true,
                    Policy {
                        entries: 2,
                        ..Default::default()
                    },
                    None
                )
                .unwrap_err()
                .contains("workspace_usage_entry_limit")
        );
        assert!(!f.0.join("workspace-usage.sqlite3").exists());
        let mut data = snapshot("first");
        data["panes"][0]["session"] = json!("x".repeat(5000));
        assert!(
            recorder
                .sample(
                    &f.0,
                    &data,
                    100.0,
                    true,
                    Policy {
                        work_bytes: 4096,
                        ..Default::default()
                    },
                    None
                )
                .unwrap_err()
                .contains("workspace_usage_work_limit_bytes")
        );
        assert!(!f.0.join("workspace-usage.sqlite3").exists());
    }

    #[test]
    fn large_expired_lease_scan_refuses_without_pruning_to_fit_and_recovers_with_saved_allowance() {
        let mut con = open(Connection::open_in_memory().unwrap()).unwrap();
        {
            let tx = con.transaction().unwrap();
            {
                let mut query = tx
                    .prepare("INSERT INTO workspace_focus VALUES(?,?,1,'old-recorder')")
                    .unwrap();
                for index in 0..3000 {
                    let key = format!("{index:064x}");
                    query.execute(params![key, key]).unwrap();
                }
            }
            tx.commit().unwrap();
        }
        let data = snapshot("current");
        for (policy, key) in [
            (
                Policy {
                    entries: 64,
                    ..Default::default()
                },
                "workspace_usage_entry_limit",
            ),
            (
                Policy {
                    entries: 4000,
                    work_bytes: 4096,
                    ..Default::default()
                },
                "workspace_usage_work_limit_bytes",
            ),
        ] {
            let mut guard = Guard::new(policy, None).unwrap();
            let prepared = prepare(&data, &mut guard, true).unwrap();
            guard.configure(&con).unwrap();
            assert!(
                observe_with(&mut con, &prepared, 200000.0, "one", true, &mut guard)
                    .unwrap_err()
                    .contains(key)
            );
            assert_eq!(
                con.query_row("SELECT COUNT(*) FROM workspace_focus", [], |r| r
                    .get::<_, i64>(0))
                    .unwrap(),
                3000
            );
            assert_eq!(
                con.query_row("SELECT COUNT(*) FROM workspace_usage", [], |r| r
                    .get::<_, i64>(0))
                    .unwrap(),
                0
            );
        }
        let mut guard = Guard::new(
            Policy {
                entries: 4000,
                work_bytes: 1048576,
                ..Default::default()
            },
            None,
        )
        .unwrap();
        let prepared = prepare(&data, &mut guard, true).unwrap();
        guard.configure(&con).unwrap();
        observe_with(&mut con, &prepared, 200000.0, "one", true, &mut guard).unwrap();
        assert_eq!(
            con.query_row("SELECT COUNT(*) FROM workspace_focus", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            1
        );
    }
    #[test]
    fn configured_busy_wait_refuses_a_locked_writer_without_retrying_or_counting_the_gap() {
        let f = Fixture::new();
        let data = snapshot("first");
        let key = key(&data["panes"][0]).unwrap();
        let mut recorder = Recorder::default();
        recorder
            .sample(&f.0, &data, 100.0, true, Policy::default(), None)
            .unwrap();
        let con =
            super::super::telegram_config::private_connection(&f.0.join("workspace-usage.sqlite3"))
                .unwrap();
        con.execute_batch("BEGIN IMMEDIATE").unwrap();
        let start = std::time::Instant::now();
        let result = recorder.sample(
            &f.0,
            &data,
            101.0,
            true,
            Policy {
                busy_ms: 0,
                ..Default::default()
            },
            None,
        );
        con.execute_batch("ROLLBACK").unwrap();
        assert!(result.unwrap_err().contains("busy"));
        assert!(start.elapsed() < std::time::Duration::from_secs(2));
        let value = recorder
            .sample(&f.0, &data, 102.0, true, Policy::default(), None)
            .unwrap()
            .unwrap();
        assert_eq!(value[&key]["seconds"], 0.0);
    }
    #[test]
    fn recording_can_be_disabled_with_the_saved_interface_setting() {
        let settings = super::super::interface_settings::InterfaceSettings::default()
            .set("workspace_usage_recording", "false")
            .unwrap();
        assert_eq!(settings.value("workspace_usage_recording").unwrap(), false);
    }
    fn snapshot(generation: &str) -> Value {
        json!({"panes":[{"backend":"herdr","session":"private-server","pane_id":"p1","terminal_id":generation}],"herdr_focus":[{"session":"private-server","pane_id":"p1"}]})
    }
    #[test]
    fn samples_exclude_outages_restarts_clock_reversal_and_new_generations() {
        let mut c = open(Connection::open_in_memory().unwrap()).unwrap();
        let s = snapshot("first");
        let k = key(&s["panes"][0]).unwrap();
        observe(&mut c, &s, 100.0, "one").unwrap();
        let v = observe(&mut c, &s, 105.0, "one").unwrap();
        assert_eq!(v[&k]["seconds"], 5.0);
        assert_eq!(v[&k]["visits"], 1);
        let v = observe(&mut c, &s, 200.0, "one").unwrap();
        assert_eq!(v[&k]["seconds"], 5.0);
        let v = observe(&mut c, &s, 201.0, "two").unwrap();
        assert_eq!(v[&k]["seconds"], 5.0);
        assert_eq!(v[&k]["visits"], 1);
        let v = observe(&mut c, &s, 190.0, "two").unwrap();
        assert_eq!(v[&k]["seconds"], 5.0);
        let v = observe(&mut c, &s, 195.0, "two").unwrap();
        assert_eq!(v[&k]["seconds"], 5.0);
        observe(&mut c, &json!({}), 202.0, "one").unwrap();
        let v = observe(&mut c, &s, 203.0, "two").unwrap();
        assert_eq!(v[&k]["seconds"], 5.0);
        let newer = snapshot("second");
        let v = observe(&mut c, &newer, 204.0, "two").unwrap();
        let newkey = key(&newer["panes"][0]).unwrap();
        assert_eq!(v[&newkey]["seconds"], 0.0);
        assert_eq!(v[&newkey]["visits"], 1);
    }
    #[test]
    fn concurrent_recorders_do_not_double_count_or_invent_visits() {
        let mut c = open(Connection::open_in_memory().unwrap()).unwrap();
        let s = snapshot("first");
        let k = key(&s["panes"][0]).unwrap();
        observe(&mut c, &s, 100.0, "one").unwrap();
        observe(&mut c, &s, 101.0, "two").unwrap();
        let v = observe(&mut c, &s, 105.0, "one").unwrap();
        assert_eq!(v[&k]["seconds"], 5.0);
        assert_eq!(v[&k]["visits"], 1);
        let v = observe(&mut c, &s, 140.0, "two").unwrap();
        assert_eq!(v[&k]["seconds"], 5.0);
        assert_eq!(v[&k]["visits"], 1);
        let v = observe(&mut c, &s, 142.0, "two").unwrap();
        assert_eq!(v[&k]["seconds"], 7.0);
    }
    #[test]
    fn conflicting_server_focus_does_not_pick_an_arbitrary_pane() {
        let mut s = snapshot("first");
        s["panes"].as_array_mut().unwrap().push(json!({"backend":"herdr","session":"private-server","pane_id":"p2","terminal_id":"second"}));
        s["herdr_focus"]
            .as_array_mut()
            .unwrap()
            .push(json!({"session":"private-server","pane_id":"p2"}));
        assert!(selected(&s).is_empty());
        s["herdr_focus"].as_array_mut().unwrap().pop();
        let f = s["herdr_focus"][0].clone();
        s["herdr_focus"].as_array_mut().unwrap().push(f);
        assert_eq!(selected(&s).len(), 1);
    }
    #[test]
    fn ambiguous_or_missing_focus_is_not_usage_and_private_names_are_not_stored() {
        let mut c = open(Connection::open_in_memory().unwrap()).unwrap();
        let mut s = snapshot("first");
        let copy = s["panes"][0].clone();
        s["panes"].as_array_mut().unwrap().push(copy);
        assert!(
            observe(&mut c, &s, 10.0, "one")
                .unwrap()
                .as_object()
                .unwrap()
                .is_empty()
        );
        s["panes"].as_array_mut().unwrap().pop();
        observe(&mut c, &s, 11.0, "one").unwrap();
        let values: String = c
            .query_row("SELECT channel || terminal FROM workspace_focus", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert!(!values.contains("private-server"));
        assert!(!values.contains("first"));
    }
}
