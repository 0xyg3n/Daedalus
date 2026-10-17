//! Local aggregate observations of Herdr's server selection, not human attention.
//! Only hashed identities, counts and timestamps are persisted; never pane text.
use crate::adapters::terminals;
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
fn open(con: Connection) -> Result<Connection> {
    con.execute_batch("CREATE TABLE IF NOT EXISTS workspace_usage(terminal TEXT PRIMARY KEY,visits INTEGER NOT NULL,seconds REAL NOT NULL,last_selected REAL NOT NULL);
    CREATE TABLE IF NOT EXISTS workspace_focus(channel TEXT PRIMARY KEY,terminal TEXT NOT NULL,observed REAL NOT NULL,owner TEXT NOT NULL);").map_err(|_|"Cannot initialize workspace usage")?;
    Ok(con)
}
fn selected(snapshot: &Value) -> BTreeMap<String, String> {
    let mut result: BTreeMap<String, Option<String>> = BTreeMap::new();
    let Some(focus) = snapshot["herdr_focus"].as_array() else {
        return BTreeMap::new();
    };
    let Some(panes) = snapshot["panes"].as_array() else {
        return BTreeMap::new();
    };
    for f in focus {
        let Some(session) = f["session"].as_str() else {
            continue;
        };
        let Some(pane) = f["pane_id"].as_str().filter(|s| !s.is_empty()) else {
            continue;
        };
        let candidates: Vec<_> = panes
            .iter()
            .filter(|p| p["backend"] == "herdr" && p["session"] == session && p["pane_id"] == pane)
            .collect();
        if candidates.len() != 1 {
            continue;
        }
        if let Some(key) = key(candidates[0]) {
            result
                .entry(hash(session.as_bytes()))
                .and_modify(|previous| {
                    if previous.as_ref() != Some(&key) {
                        *previous = None;
                    }
                })
                .or_insert(Some(key));
        }
    }
    result
        .into_iter()
        .filter_map(|(channel, key)| Some((channel, key?)))
        .collect()
}
fn observe(con: &mut Connection, snapshot: &Value, now: f64, owner: &str) -> Result<Value> {
    if !now.is_finite() || now < 0.0 {
        return Err("Invalid usage timestamp".into());
    }
    let samples = selected(snapshot);
    let tx = con
        .transaction()
        .map_err(|_| "Cannot lock workspace usage")?;
    // Remove absent channels even after partial discovery: never count an outage.
    let previous: BTreeMap<String, (String, f64, String)> = {
        let mut q = tx
            .prepare("SELECT channel,terminal,observed,owner FROM workspace_focus")
            .map_err(|_| "Cannot read workspace usage")?;
        q.query_map([], |r| Ok((r.get(0)?, (r.get(1)?, r.get(2)?, r.get(3)?))))
            .map_err(|_| "Cannot read workspace usage")?
            .collect::<std::result::Result<_, _>>()
            .map_err(|_| "Invalid workspace usage")?
    };
    tx.execute("DELETE FROM workspace_focus WHERE owner=?", [owner])
        .map_err(|_| "Cannot update workspace usage")?;
    for (channel, terminal) in samples {
        let old = previous.get(&channel);
        // A fresh recorder owns this endpoint. Another TUI must not count it twice.
        if old.is_some_and(|(_, at, run)| run != owner && now - *at <= 30.0) {
            continue;
        }
        let same = old.is_some_and(|(id, _, run)| id == &terminal && run == owner);
        let changed = old.is_none_or(|(id, _, _)| id != &terminal);
        let elapsed = old
            .filter(|(_, at, _)| same && now >= *at && now - *at <= 30.0)
            .map_or(0.0, |(_, at, _)| now - at);
        tx.execute("INSERT INTO workspace_usage VALUES(?1,?2,?3,?4) ON CONFLICT(terminal) DO UPDATE SET visits=visits+excluded.visits,seconds=seconds+excluded.seconds,last_selected=MAX(last_selected,excluded.last_selected)",params![terminal,i64::from(changed),elapsed,now]).map_err(|_|"Cannot save workspace usage")?;
        tx.execute(
            "INSERT OR REPLACE INTO workspace_focus VALUES(?,?,?,?)",
            params![
                channel,
                terminal,
                old.map_or(now, |(_, at, _)| now.max(*at)),
                owner
            ],
        )
        .map_err(|_| "Cannot save workspace usage")?;
    }
    tx.commit().map_err(|_| "Cannot save workspace usage")?;
    let mut result = serde_json::Map::new();
    // Return current terminals only; old generations cannot claim their usage.
    if let Some(panes) = snapshot["panes"].as_array() {
        for pane in panes {
            let Some(id) = key(pane) else { continue };
            let row = con
                .query_row(
                    "SELECT visits,seconds,last_selected FROM workspace_usage WHERE terminal=?",
                    [&id],
                    |r| {
                        Ok((
                            r.get::<_, i64>(0)?,
                            r.get::<_, f64>(1)?,
                            r.get::<_, f64>(2)?,
                        ))
                    },
                )
                .optional()
                .map_err(|_| "Cannot read workspace usage")?;
            if let Some((visits, seconds, last)) = row {
                result.insert(
                    id,
                    json!({"visits":visits,"seconds":seconds,"last_selected":last}),
                );
            }
        }
    }
    Ok(Value::Object(result))
}
pub fn sample(directory: &Path, snapshot: &Value, now: f64) -> Result<Value> {
    let mut con = open(super::telegram_config::private_connection(
        &directory.join("workspace-usage.sqlite3"),
    )?)?;
    observe(&mut con, snapshot, now, run())
}
#[cfg(test)]
mod tests {
    use super::*;
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
