//! Receipt-backed tmux session renames. Provider conversations and Armonia
//! identity/alias records are distinct and are never changed by this operation.
use super::telegram_commands::Caller;
use crate::{
    adapters::{
        terminal_actions::Receipt,
        terminals::{self, Terminals},
    },
    core::{identity::Backend, routes::Scope},
};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{collections::BTreeSet, path::Path};
type Result<T> = std::result::Result<T, String>;
pub fn name(value: &str) -> Result<String> {
    if value.is_empty()
        || value.len() > 64
        || !value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_-".contains(&b))
    {
        return Err("Use a session name of 1–64 letters, digits, underscores or hyphens".into());
    }
    Ok(value.into())
}
pub fn operation(value: &str) -> Result<String> {
    if value.len() != 67
        || !value.starts_with("sr:")
        || !value[3..].bytes().all(|b| b.is_ascii_hexdigit())
    {
        return Err("Use the exact sr: operation ID from the rename receipt".into());
    }
    Ok(value.into())
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Observation {
    pub endpoint: String,
    pub session: String,
    pub server_generation: String,
    pub label: String,
    /// Sorted pane generation/window membership and attached client identities.
    pub members: BTreeSet<(String, String, String)>,
    pub clients: BTreeSet<(String, String, String)>,
}
impl Observation {
    fn same_identity(&self, other: &Self) -> bool {
        self.endpoint == other.endpoint
            && self.session == other.session
            && self.server_generation == other.server_generation
            && self.members == other.members
            && self.clients == other.clients
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Plan {
    pub scope: Scope,
    pub actor: String,
    pub before: Observation,
    pub pane: Value,
    pub name: String,
    pub operation: String,
}
impl Plan {
    pub fn description(&self) -> String {
        format!(
            "Rename tmux session {} to {}. All {} panes and {} attached clients share this session name. Agent and provider conversation names are unchanged. Operation: {}",
            self.before.session,
            self.name,
            self.before.members.len(),
            self.before.clients.len(),
            self.operation
        )
    }
}
pub trait BackendApi {
    fn observe(&mut self, pane: &Value) -> Result<Observation>;
    fn rename(&mut self, pane: &Value, name: &str) -> Result<()>;
}
pub struct Native<'a>(pub &'a Terminals);
impl BackendApi for Native<'_> {
    fn observe(&mut self, pane: &Value) -> Result<Observation> {
        let actual = self.0.verify(pane)?;
        let id = terminals::identity(&actual)?;
        if id.backend != Backend::Tmux {
            return Err("Session rename requires an exact tmux route".into());
        }
        let mut inventory = self.0.tmux_inventory(&id.endpoint)?;
        for client in inventory["clients"]
            .as_array_mut()
            .ok_or("Missing client inventory")?
        {
            if client["session_id"] != actual["session_id"] {
                continue;
            }
            let pid = client["pid"]
                .as_str()
                .ok_or("Missing client process identity")?;
            let start = terminals::process_start(&self.0.proc_root, pid)
                .ok_or("Unverified attached client process")?;
            client["start_ticks"] = start.into();
        }
        observe_inventory(&inventory, &actual)
    }
    fn rename(&mut self, pane: &Value, name: &str) -> Result<()> {
        let Receipt { .. } =
            super::workspace_actions::organize(self.0, pane, "rename-project", name, None)?;
        // Adapter acknowledgement is never considered proof. Caller always
        // checks actual session name and membership, including uncertain ACKs.
        Ok(())
    }
}
fn observe_inventory(inventory: &Value, pane: &Value) -> Result<Observation> {
    let id = terminals::identity(pane)?;
    if id.backend != Backend::Tmux {
        return Err("Session rename requires tmux".into());
    }
    let session = pane["session_id"]
        .as_str()
        .filter(|s| s.starts_with('$') && s[1..].bytes().all(|b| b.is_ascii_digit()) && s.len() > 1)
        .ok_or("Invalid session identity")?;
    let mut members = BTreeSet::new();
    let mut labels = BTreeSet::new();
    let mut generations = BTreeSet::new();
    let mut target = false;
    for row in inventory["panes"]
        .as_array()
        .ok_or("Missing session inventory")?
    {
        if row["session_id"] != session {
            continue;
        }
        let current = terminals::identity(row)?;
        if current.endpoint != id.endpoint {
            return Err("Mixed terminal server inventory".into());
        }
        target |= current == id;
        let parts: Vec<_> = current.generation.split(':').collect();
        if parts.len() != 5
            || parts[0] != "v2"
            || parts[1..]
                .iter()
                .any(|p| p.is_empty() || !p.bytes().all(|b| b.is_ascii_digit()))
        {
            return Err("Missing verified tmux process generation".into());
        }
        generations.insert(format!("{}:{}", parts[1], parts[2]));
        let label = row["session"].as_str().ok_or("Missing session name")?;
        labels.insert(label.to_owned());
        let window = row["window_id"]
            .as_str()
            .filter(|s| {
                s.starts_with('@') && s.len() > 1 && s[1..].bytes().all(|b| b.is_ascii_digit())
            })
            .ok_or("Invalid window identity")?;
        members.insert((current.pane, current.generation, window.into()));
    }
    if !target || labels.len() != 1 || generations.len() != 1 || members.is_empty() {
        return Err("Session inventory is missing or ambiguous".into());
    }
    let mut clients = BTreeSet::new();
    for client in inventory["clients"]
        .as_array()
        .ok_or("Missing client inventory")?
    {
        if client["session_id"] != session {
            continue;
        }
        let pid = client["pid"]
            .as_str()
            .filter(|s| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()))
            .ok_or("Missing client identity")?;
        clients.insert((
            format!(
                "{}:{}",
                pid,
                client["start_ticks"]
                    .as_str()
                    .filter(|s| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()))
                    .ok_or("Missing client generation")?
            ),
            client["client"]
                .as_str()
                .ok_or("Missing client name")?
                .into(),
            client["tty"].as_str().ok_or("Missing client TTY")?.into(),
        ));
    }
    Ok(Observation {
        endpoint: id.endpoint,
        session: session.into(),
        server_generation: generations.into_iter().next().unwrap(),
        label: labels.into_iter().next().unwrap(),
        members,
        clients,
    })
}
pub fn prepare(
    caller: &Caller,
    pane: &Value,
    new_name: &str,
    backend: &mut impl BackendApi,
) -> Result<Plan> {
    let name = name(new_name)?;
    if caller.event_id.is_empty() {
        return Err("Rename requires stable event identity".into());
    }
    let before = backend.observe(pane)?;
    let id = terminals::identity(pane)?;
    if id.backend != Backend::Tmux || id.endpoint != before.endpoint {
        return Err("Invalid rename target".into());
    }
    // Only transport identity and the exact selected session/window hint are saved.
    let pane = json!({"backend":"tmux","socket":id.endpoint,"pane_id":id.pane,"terminal_id":id.generation,"session_id":before.session,"window_id":pane["window_id"]});
    let operation = format!(
        "sr:{:x}",
        Sha256::digest(
            serde_json::to_vec(&(
                &caller.scope,
                &caller.actor,
                &caller.event_id,
                &before,
                &name
            ))
            .map_err(|_| "Cannot encode rename review")?
        )
    );
    Ok(Plan {
        scope: caller.scope.clone(),
        actor: caller.actor.clone(),
        before,
        pane,
        name,
        operation,
    })
}
const SCHEMA: &str = "PRAGMA synchronous=FULL; CREATE TABLE IF NOT EXISTS session_renames(operation TEXT PRIMARY KEY,scope TEXT NOT NULL,actor TEXT NOT NULL,plan TEXT NOT NULL,state TEXT NOT NULL); CREATE INDEX IF NOT EXISTS session_rename_states ON session_renames(state);";
pub struct Store {
    con: Connection,
}
impl Store {
    pub fn open(directory: &Path) -> Result<Self> {
        Self::from_connection(super::telegram_config::private_connection(
            &directory.join("session-renames.sqlite3"),
        )?)
    }
    fn from_connection(con: Connection) -> Result<Self> {
        con.execute_batch(SCHEMA)
            .map_err(|_| "Cannot initialize rename journal")?;
        Ok(Self { con })
    }
    pub fn apply(&mut self, plan: &Plan, backend: &mut impl BackendApi) -> Result<String> {
        name(&plan.name)?;
        operation(&plan.operation)?;
        let current = backend.observe(&plan.pane)?;
        if current != plan.before {
            return Err("Session identity, name or membership changed; review again".into());
        }
        let encoded = serde_json::to_string(plan).map_err(|_| "Cannot encode rename intent")?;
        let scope = serde_json::to_string(&plan.scope).map_err(|_| "Cannot encode rename scope")?;
        let tx = self
            .con
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .map_err(|_| "Rename journal unavailable")?;
        if tx
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM session_renames WHERE operation=?)",
                [&plan.operation],
                |r| r.get::<_, bool>(0),
            )
            .map_err(|_| "Rename journal unavailable")?
        {
            return Err("Rename already attempted; inspect its status instead".into());
        }
        let mut query = tx
            .prepare(
                "SELECT plan FROM session_renames WHERE state IN ('intent','unknown') LIMIT 10001",
            )
            .map_err(|_| "Rename journal unavailable")?;
        let rows = query
            .query_map([], |r| r.get::<_, String>(0))
            .map_err(|_| "Rename journal unavailable")?;
        for (i, row) in rows.enumerate() {
            if i >= 10000 {
                return Err("Rename review backlog requires local inspection".into());
            }
            let old: Plan = serde_json::from_str(&row.map_err(|_| "Rename journal unavailable")?)
                .map_err(|_| "Invalid prior rename")?;
            if old.before.endpoint == plan.before.endpoint
                && old.before.session == plan.before.session
                && old.before.server_generation == plan.before.server_generation
            {
                return Err("Earlier rename is uncertain; inspect its status first".into());
            }
        }
        drop(query);
        tx.execute(
            "INSERT INTO session_renames VALUES(?,?,?,?,'intent')",
            params![plan.operation, scope, plan.actor, encoded],
        )
        .map_err(|_| "Cannot persist rename intent")?;
        tx.commit().map_err(|_| "Cannot persist rename intent")?;
        // Never retry an attempted effect, even if the adapter reports an error.
        let _outcome = backend.rename(&plan.pane, &plan.name);
        self.reconcile(plan, backend)
    }
    fn reconcile(&mut self, plan: &Plan, backend: &mut impl BackendApi) -> Result<String> {
        let verified = backend
            .observe(&plan.pane)
            .is_ok_and(|after| after.label == plan.name && plan.before.same_identity(&after));
        let state = if verified { "verified" } else { "unknown" };
        self.con
            .execute(
                "UPDATE session_renames SET state=? WHERE operation=?",
                params![state, plan.operation],
            )
            .map_err(|_| "Cannot persist rename observation; inspect status before retrying")?;
        Ok(format!(
            "Session rename {state}. Operation: {}. {}",
            plan.operation,
            if verified {
                "Session identity, panes, windows and attached clients verified; provider conversation unchanged."
            } else {
                "No repeat attempted. Use /session_rename_status with this operation ID."
            }
        ))
    }
    pub fn status(
        &mut self,
        caller: &Caller,
        operation_id: &str,
        backend: &mut impl BackendApi,
    ) -> Result<String> {
        operation(operation_id)?;
        let scope = serde_json::to_string(&caller.scope).map_err(|_| "Invalid rename scope")?;
        let row: Option<String> = self
            .con
            .query_row(
                "SELECT plan FROM session_renames WHERE operation=? AND scope=? AND actor=?",
                params![operation_id, scope, caller.actor],
                |r| r.get(0),
            )
            .optional()
            .map_err(|_| "Rename journal unavailable")?;
        let plan: Plan = serde_json::from_str(
            &row.ok_or("Rename operation is unavailable for this actor and chat")?,
        )
        .map_err(|_| "Invalid saved rename")?;
        self.reconcile(&plan, backend)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn caller() -> Caller {
        Caller {
            scope: Scope {
                instance: "fixture".into(),
                bot: "123".into(),
                chat: "42".into(),
            },
            actor: "7".into(),
            human: true,
            forwarded: false,
            bot_username: "fixture_bot".into(),
            event_id: "1".into(),
        }
    }
    fn pane() -> Value {
        json!({"backend":"tmux","socket":"/tmp/test.sock","session_id":"$1","session":"project","window_id":"@1","pane_id":"%1","terminal_id":"v2:100:111:200:222"})
    }
    fn inventory() -> Value {
        json!({"panes":[pane()],"clients":[{"session_id":"$1","pid":"333","start_ticks":"444","client":"/dev/pts/1","tty":"/dev/pts/1"}]})
    }
    struct Mock {
        observation: Observation,
        mutations: usize,
        change: bool,
        lose_ack: bool,
    }
    impl BackendApi for Mock {
        fn observe(&mut self, _: &Value) -> Result<Observation> {
            Ok(self.observation.clone())
        }
        fn rename(&mut self, _: &Value, label: &str) -> Result<()> {
            self.mutations += 1;
            if self.change {
                self.observation.label = label.into();
            }
            if self.lose_ack {
                Err("Uncertain fixture response".into())
            } else {
                Ok(())
            }
        }
    }
    fn mock() -> Mock {
        Mock {
            observation: observe_inventory(&inventory(), &pane()).unwrap(),
            mutations: 0,
            change: true,
            lose_ack: false,
        }
    }
    fn store() -> Store {
        Store::from_connection(Connection::open_in_memory().unwrap()).unwrap()
    }
    #[test]
    fn native_adapter_renames_fixture_session_and_checks_readback() {
        use std::{
            fs,
            os::unix::fs::PermissionsExt,
            time::{SystemTime, UNIX_EPOCH},
        };
        let root = std::env::temp_dir().join(format!(
            "daedalus-rename-native-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir(&root).unwrap();
        let proc = root.join("proc");
        for (pid, start) in [(100, 111), (200, 222)] {
            let directory = proc.join(pid.to_string());
            fs::create_dir_all(&directory).unwrap();
            let mut fields = vec!["0".to_owned(); 20];
            fields[0] = "S".into();
            fields[19] = start.to_string();
            fs::write(
                directory.join("stat"),
                format!("{pid} (fixture) {}", fields.join(" ")),
            )
            .unwrap();
        }
        fs::write(root.join("label"), "project").unwrap();
        let script = root.join("tmux");
        let template = r#"#!/bin/sh
cd '__ROOT__' || exit 1
case "$3" in
list-panes) label=$(cat label); printf '%s\t$1\twindow\t@1\t%%1\t200\tclaude\t1\t/tmp\t100\ttitle\t/dev/pts/1\t\t0\t0\t80\t24\t0\t0\t0\t\n' "$label" ;;
list-clients) exit 0 ;;
rename-session) printf '%s' "$6" > label; printf 'attempt\n' >> attempts ;;
*) exit 1 ;;
esac
"#;
        fs::write(
            &script,
            template.replace("__ROOT__", root.to_str().unwrap()),
        )
        .unwrap();
        fs::set_permissions(&script, fs::Permissions::from_mode(0o700)).unwrap();
        let adapter = Terminals {
            tmux: script,
            proc_root: proc,
            ..Terminals::default()
        };
        let mut backend = Native(&adapter);
        let plan = prepare(&caller(), &pane(), "new-project", &mut backend).unwrap();
        let mut journal = store();
        assert!(
            journal
                .apply(&plan, &mut backend)
                .unwrap()
                .contains("rename verified")
        );
        assert_eq!(
            fs::read_to_string(root.join("label")).unwrap(),
            "new-project"
        );
        assert!(
            journal
                .status(&caller(), &plan.operation, &mut backend)
                .unwrap()
                .contains("rename verified")
        );
        assert_eq!(
            fs::read_to_string(root.join("attempts")).unwrap(),
            "attempt\n"
        );
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn actual_name_and_membership_prove_success_even_when_ack_is_lost() {
        let mut backend = mock();
        backend.lose_ack = true;
        let plan = prepare(&caller(), &pane(), "renamed", &mut backend).unwrap();
        let mut journal = store();
        let receipt = journal.apply(&plan, &mut backend).unwrap();
        assert!(receipt.contains("rename verified"));
        assert_eq!(backend.mutations, 1);
        assert!(journal.apply(&plan, &mut backend).is_err());
        assert!(
            journal
                .status(&caller(), &plan.operation, &mut backend)
                .unwrap()
                .contains("rename verified")
        );
        assert_eq!(backend.mutations, 1);
    }
    #[test]
    fn unknown_is_not_replayed_and_status_is_actor_chat_scoped() {
        let mut backend = mock();
        backend.change = false;
        let plan = prepare(&caller(), &pane(), "renamed", &mut backend).unwrap();
        let mut journal = store();
        assert!(
            journal
                .apply(&plan, &mut backend)
                .unwrap()
                .contains("rename unknown")
        );
        let mut next = caller();
        next.event_id = "2".into();
        let next_plan = prepare(&next, &pane(), "another", &mut backend).unwrap();
        assert!(journal.apply(&next_plan, &mut backend).is_err());
        let mut other = caller();
        other.actor = "8".into();
        assert!(
            journal
                .status(&other, &plan.operation, &mut backend)
                .is_err()
        );
        other = caller();
        other.scope.chat = "99".into();
        assert!(
            journal
                .status(&other, &plan.operation, &mut backend)
                .is_err()
        );
        assert_eq!(backend.mutations, 1);
        backend.observation.label = "renamed".into();
        assert!(
            journal
                .status(&caller(), &plan.operation, &mut backend)
                .unwrap()
                .contains("rename verified")
        );
        assert_eq!(backend.mutations, 1);
    }
    #[test]
    fn session_or_client_changes_during_confirmation_block_mutation() {
        for field in ["client", "pane", "server", "label"] {
            let mut backend = mock();
            let plan = prepare(&caller(), &pane(), "renamed", &mut backend).unwrap();
            match field {
                "client" => backend.observation.clients.clear(),
                "pane" => backend.observation.members.clear(),
                "server" => backend.observation.server_generation = "recycled".into(),
                _ => backend.observation.label = "external-rename".into(),
            }
            assert!(store().apply(&plan, &mut backend).is_err(), "{field}");
            assert_eq!(backend.mutations, 0);
        }
    }
    #[test]
    fn partial_receipt_never_becomes_verified_and_inventory_rejects_ambiguous_evidence() {
        let mut backend = mock();
        let plan = prepare(&caller(), &pane(), "renamed", &mut backend).unwrap();
        let mut journal = store();
        journal.apply(&plan, &mut backend).unwrap();
        backend.observation.clients.clear();
        assert!(
            journal
                .status(&caller(), &plan.operation, &mut backend)
                .unwrap()
                .contains("rename unknown")
        );
        let mut rows = inventory();
        rows["clients"][0]["start_ticks"] = Value::Null;
        assert!(observe_inventory(&rows, &pane()).is_err());
        let mut other = pane();
        other["terminal_id"] = "v2:100:999:300:444".into();
        other["pane_id"] = "%2".into();
        let rows = json!({"panes":[pane(),other],"clients":[]});
        assert!(observe_inventory(&rows, &pane()).is_err());
        for value in ["", "two names", ";touch", "../name"] {
            assert!(name(value).is_err());
        }
    }
}
