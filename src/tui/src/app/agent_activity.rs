//! Read-only lifecycle feedback from already verified local hook scopes.
//! These observations never authorize terminal input or goal continuation.
use super::{hook_runtime::Registration, runtime_settings::Settings};
use crate::{adapters::terminals, core::layouts};
use rusqlite::{Connection, OpenFlags, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    path::Path,
    time::{Duration, Instant},
};
type Result<T> = std::result::Result<T, String>;

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Observation {
    pub state: String,
    pub detail: String,
    pub source: String,
    pub observed_at: f64,
    pub expires_at: f64,
}
impl Observation {
    pub fn fresh(&self, now: f64) -> bool {
        now.is_finite()
            && self.observed_at.is_finite()
            && self.expires_at.is_finite()
            && self.observed_at > 0.0
            && self.observed_at <= now
            && now <= self.expires_at
    }
}
fn open(path: &Path, deadline: Instant) -> Result<Option<Connection>> {
    if !path.exists() {
        return Ok(None);
    }
    layouts::check_path(path)?;
    let _held = super::hook_runtime::private_file(path)
        .map_err(|_| "Agent activity database is not private")?;
    let con = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY
            | OpenFlags::SQLITE_OPEN_NOFOLLOW
            | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|_| "Cannot read agent activity database")?;
    con.busy_timeout(Duration::ZERO)
        .map_err(|_| "Cannot bound agent activity database")?;
    con.progress_handler(1000, Some(move || Instant::now() >= deadline))
        .map_err(|_| "Cannot bound agent activity query")?;
    con.execute_batch("PRAGMA trusted_schema=OFF; PRAGMA query_only=ON;")
        .map_err(|_| "Cannot restrict agent activity query")?;
    Ok(Some(con))
}
fn detail(event: &str, tool: Option<&str>, state: &str) -> String {
    match (event, state) {
        ("PreToolUse", "working") => tool
            .map(|name| format!("Using {name}"))
            .unwrap_or_else(|| "Using a tool".into()),
        ("PostToolUse", "working") => tool
            .map(|name| format!("Finished {name}; turn continues"))
            .unwrap_or_else(|| "Tool finished; turn continues".into()),
        ("UserPromptSubmit", "working") => "Processing a prompt".into(),
        ("Activity", "working") => "Agent reported activity".into(),
        ("Stop" | "Settled", "idle") => "Turn settled".into(),
        (_, "working") => "Agent reported working".into(),
        (_, "idle") => "Agent reported idle".into(),
        _ => "Lifecycle state is not settled".into(),
    }
}
fn observe(
    con: &Connection,
    delivery: Option<&Connection>,
    registration: &Registration,
    settings: &Settings,
    now: f64,
) -> Result<Option<Observation>> {
    let scope =
        serde_json::to_string(&registration.scope).map_err(|_| "Cannot encode activity scope")?;
    let row:Option<(String,i64,String,bool)>=con.query_row(
        "SELECT state,observed_at,event_id,finalized FROM hook_observations WHERE scope=? ORDER BY seq DESC LIMIT 1",[scope],
        |row|Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?))).optional().map_err(|_|"Agent lifecycle observation is unavailable")?;
    let Some((state, time, key, finalized)) = row else {
        return Ok(None);
    };
    let at = time as f64 / 1000.0;
    if time < 0 || at > now {
        return Ok(Some(Observation {
            state: "unknown".into(),
            detail: "Agent event timestamp is invalid".into(),
            source: "verified hook".into(),
            ..Observation::default()
        }));
    }
    let state = if !finalized {
        "unknown"
    } else {
        match state.as_str() {
            "\"busy\"" => "working",
            "\"idle\"" => "idle",
            _ => "unknown",
        }
    };
    // Fetch only event/tool metadata by its exact admitted event key. Never
    // materialize prompts, replies, transcripts, credentials, or tool arguments.
    let metadata:Option<(Option<String>,Option<String>)>=delivery.and_then(|db|db.query_row(
        "SELECT substr(json_extract(payload,'$.event'),1,64),CASE WHEN length(json_extract(payload,'$.tool_name'))<=128 THEN json_extract(payload,'$.tool_name') END FROM inbox WHERE channel='hooks' AND bot=? AND event_id=? AND length(payload)<=? AND json_valid(payload) AND json_extract(payload,'$.registration_id')=? AND json_extract(payload,'$.scope.conversation')=?",
        params![registration.scope.instance,key,settings.harness_probe_limit_bytes as i64,registration.id,registration.scope.conversation],
        |row|Ok((row.get(0)?,row.get(1)?))).optional().ok().flatten());
    let (event, tool) = metadata.unwrap_or_default();
    let tool = tool
        .as_deref()
        .filter(|name| !name.is_empty() && !name.chars().any(char::is_control));
    Ok(Some(Observation {
        state: state.into(),
        detail: detail(event.as_deref().unwrap_or(""), tool, state),
        source: "verified hook".into(),
        observed_at: at,
        expires_at: at + settings.agent_activity_max_age_ms as f64 / 1000.0,
    }))
}
pub(crate) fn enrich(
    directory: &Path,
    snapshot: &mut Value,
    verified: &[Registration],
    settings: &Settings,
    deadline: Instant,
    now: f64,
    budget: Option<&super::discovery_budget::Budget>,
) -> Result<()> {
    let Some(panes) = snapshot["panes"].as_array_mut() else {
        return Ok(());
    };
    for pane in panes.iter_mut() {
        if let Some(map) = pane.as_object_mut() {
            map.remove("agent_activity");
        }
    }
    if verified.is_empty() {
        return Ok(());
    }
    let deadline = deadline
        .min(Instant::now() + Duration::from_millis(settings.agent_activity_query_timeout_ms));
    let Some(con) = open(&directory.join("hook-observations.sqlite3"), deadline)? else {
        return Ok(());
    };
    let delivery = open(&directory.join("delivery.sqlite3"), deadline)?;
    for pane in panes {
        if Instant::now() >= deadline {
            return Err("Agent activity query deadline reached".into());
        }
        if crate::adapters::pane_observations::conflicted(pane) {
            continue;
        }
        let Ok(identity) = terminals::identity(pane) else {
            continue;
        };
        let mut matches = verified.iter().filter(|r| r.scope.terminal == identity);
        let Some(registration) = matches.next() else {
            continue;
        };
        if matches.next().is_some()
            || terminals::reported_session(pane) != Some(registration.scope.conversation.as_str())
        {
            continue;
        }
        if let Some(observed) = observe(&con, delivery.as_ref(), registration, settings, now)? {
            let value = json!(observed);
            if let Some(budget) = budget {
                budget.admit_value(&value)?;
            }
            pane["agent_activity"] = value;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    struct Fixture {
        root: std::path::PathBuf,
        registration: Registration,
        pane: Value,
    }
    impl Fixture {
        fn new() -> Self {
            let root = std::env::temp_dir().join(format!(
                "daedalus-agent-activity-{}-{}",
                std::process::id(),
                crate::test_support::unique_nanos()
            ));
            let registration:Registration=serde_json::from_value(json!({"version":1,"id":"fixture","directory":root,
                "scope":{"instance":"fixture","terminal":{"backend":"herdr","endpoint":"fixture","pane":"pane","generation":"generation"},"harness":"claude","conversation":"12345678-abcd-4000-8000-123456789abc"},
                "goal_id":null,"transcript":null,"events":["PreToolUse","Stop"],"options":{"telegram_delivery":false,"telegram_progress":false,"notifications":false,"hindsight":false},"activity_lease_ms":60000,"pending_timeout_ms":30000,"hook_timeout_ms":1000})).unwrap();
            for name in ["hook-observations.sqlite3", "delivery.sqlite3"] {
                layouts::write_private(&root.join(name), b"").unwrap();
            }
            let con = Connection::open(root.join("hook-observations.sqlite3")).unwrap();
            con.execute_batch("CREATE TABLE hook_observations(seq INTEGER PRIMARY KEY,scope TEXT,event_id TEXT,state TEXT,observed_at INTEGER,finalized INTEGER);CREATE INDEX activity_scope ON hook_observations(scope,seq);").unwrap();
            con.execute(
                "INSERT INTO hook_observations VALUES(1,?,'event','\"busy\"',1000000,1)",
                [serde_json::to_string(&registration.scope).unwrap()],
            )
            .unwrap();
            let con = Connection::open(root.join("delivery.sqlite3")).unwrap();
            con.execute_batch("CREATE TABLE inbox(channel TEXT,bot TEXT,event_id TEXT,payload TEXT,UNIQUE(channel,bot,event_id));").unwrap();
            con.execute("INSERT INTO inbox VALUES('hooks','fixture','event',?)",[json!({"registration_id":"fixture","scope":registration.scope,"event":"PreToolUse","tool_name":"exec_command","final_message":"PRIVATE_REPLY_MUST_NOT_BE_PROJECTED","prompt":"PRIVATE_PROMPT_MUST_NOT_BE_PROJECTED","tool_input":{"command":"PRIVATE_COMMAND_MUST_NOT_BE_PROJECTED"}}).to_string()]).unwrap();
            let pane = json!({"backend":"herdr","session":"fixture","pane_id":"pane","terminal_id":"generation","agent":"claude","agent_session":{"agent":"claude","kind":"id","value":registration.scope.conversation}});
            Self {
                root,
                registration,
                pane,
            }
        }
        fn enrich(&self, panes: Vec<Value>, verified: &[Registration]) -> Value {
            let mut snapshot = json!({"panes":panes});
            enrich(
                &self.root,
                &mut snapshot,
                verified,
                &Settings::default(),
                Instant::now() + Duration::from_secs(1),
                1001.0,
                None,
            )
            .unwrap();
            snapshot
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }
    #[test]
    fn exact_hook_activity_and_tool_metadata_are_projected_without_writes_or_private_content() {
        let f = Fixture::new();
        let before: Vec<_> = ["hook-observations.sqlite3", "delivery.sqlite3"]
            .into_iter()
            .map(|name| std::fs::read(f.root.join(name)).unwrap())
            .collect();
        let snapshot = f.enrich(vec![f.pane.clone()], std::slice::from_ref(&f.registration));
        let observed: Observation =
            serde_json::from_value(snapshot["panes"][0]["agent_activity"].clone()).unwrap();
        assert_eq!(observed.state, "working");
        assert_eq!(observed.detail, "Using exec_command");
        assert!(observed.fresh(1001.0));
        assert!(!observed.fresh(1061.0));
        assert!(!snapshot.to_string().contains("MUST_NOT_BE_PROJECTED"));
        for (index, name) in ["hook-observations.sqlite3", "delivery.sqlite3"]
            .into_iter()
            .enumerate()
        {
            assert_eq!(std::fs::read(f.root.join(name)).unwrap(), before[index]);
        }
        let con = open(
            &f.root.join("hook-observations.sqlite3"),
            Instant::now() + Duration::from_secs(1),
        )
        .unwrap()
        .unwrap();
        assert!(con.execute_batch("DELETE FROM hook_observations;").is_err());
    }
    #[test]
    fn switched_conversations_generations_and_duplicate_registrations_never_inherit_activity() {
        let f = Fixture::new();
        for key in ["terminal_id", "agent_session"] {
            let mut pane = f.pane.clone();
            pane[key] = json!("different");
            let snapshot = f.enrich(vec![pane], std::slice::from_ref(&f.registration));
            assert!(snapshot["panes"][0].get("agent_activity").is_none());
        }
        let duplicate = f.enrich(
            vec![f.pane.clone()],
            &[f.registration.clone(), f.registration.clone()],
        );
        assert!(duplicate["panes"][0].get("agent_activity").is_none());
    }
    #[test]
    fn unfinalized_idle_is_unknown_and_observation_admission_uses_remaining_budget() {
        let f = Fixture::new();
        let con = Connection::open(f.root.join("hook-observations.sqlite3")).unwrap();
        con.execute_batch("UPDATE hook_observations SET state='\"idle\"',finalized=0;")
            .unwrap();
        drop(con);
        let snapshot = f.enrich(vec![f.pane.clone()], std::slice::from_ref(&f.registration));
        assert_eq!(snapshot["panes"][0]["agent_activity"]["state"], "unknown");
        let settings = Settings::default();
        let budget = super::super::discovery_budget::Budget::new(&settings).unwrap();
        budget.admit_text(budget.remaining_bytes(), 0).unwrap();
        let mut snapshot = json!({"panes":[f.pane]});
        assert!(
            enrich(
                &f.root,
                &mut snapshot,
                std::slice::from_ref(&f.registration),
                &settings,
                budget.deadline,
                1001.0,
                Some(&budget)
            )
            .is_err()
        );
        assert!(snapshot["panes"][0].get("agent_activity").is_none());
    }
}
