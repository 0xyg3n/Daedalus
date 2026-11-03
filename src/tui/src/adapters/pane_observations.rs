//! Repeated full views are observations, never independent terminal authority.
use crate::core::identity::{Backend, TerminalId};
use serde_json::{Value, json};
use std::{
    borrow::Cow,
    collections::{HashMap, HashSet},
};

pub const WARNING: &str = "Conflicting terminal observations; known identities remain visible, but refresh before using terminal actions or geometry.";
pub fn conflicted(pane: &Value) -> bool {
    pane["observation_conflict"] == true
}
fn text<'a>(row: &'a Value, key: &str) -> &'a str {
    row.get(key).and_then(Value::as_str).unwrap_or("")
}
fn first(row: &Value, keys: &[&str]) -> String {
    keys.iter()
        .map(|key| text(row, key))
        .find(|value| !value.is_empty())
        .unwrap_or("")
        .into()
}
fn location(pane: &Value, id: &TerminalId) -> [Value; 2] {
    let project = match id.backend {
        Backend::Herdr | Backend::Native => {
            pane.get("workspace_id").cloned().unwrap_or(Value::Null)
        }
        Backend::Tmux => json!(first(pane, &["session_id", "session"])),
    };
    let group = if id.backend == Backend::Herdr {
        pane.get("tab_id").cloned().unwrap_or(Value::Null)
    } else {
        let group = first(pane, &["window_id", "window"]);
        json!(if group.is_empty() { "?".into() } else { group })
    };
    [project, group]
}
fn uncertain(pane: &Value, id: &TerminalId) -> Value {
    let [project, group] = location(pane, id);
    let mut result = json!({"backend":id.backend,"pane_id":id.pane,"terminal_id":id.generation,
        "observation_conflict":true,"title":"Conflicting terminal observations"});
    match id.backend {
        Backend::Tmux => {
            result["socket"] = json!(id.endpoint);
            result["server"] = json!(id.endpoint);
            result["session_id"] = project.clone();
            result["session"] = project;
            result["window_id"] = group.clone();
            result["window"] = group;
        }
        Backend::Herdr => {
            result["session"] = json!(id.endpoint);
            result["server"] = json!(id.endpoint);
            result["workspace_id"] = project;
            result["tab_id"] = group;
        }
        Backend::Native => {
            result["session"] = json!(id.endpoint);
            result["server"] = json!("local");
            result["workspace_id"] = project;
            result["window_id"] = group;
        }
    }
    result
}
pub struct Index<'a> {
    pub panes: Vec<Cow<'a, Value>>,
    pub conflicts: usize,
}
pub fn index(rows: &[Value]) -> Index<'_> {
    let mut seen = HashMap::new();
    let mut kept = Vec::new();
    let mut conflicts = HashSet::new();
    for (position, pane) in rows.iter().enumerate() {
        let identity = super::terminals::identity(pane).ok();
        if let Some(id) = &identity {
            if conflicted(pane) {
                conflicts.insert(id.clone());
            }
            let key = (
                id.clone(),
                serde_json::to_string(&location(pane, id)).expect("View identity values serialize"),
            );
            if let Some(previous) = seen.get(&key).copied() {
                if rows[previous] != *pane {
                    conflicts.insert(id.clone());
                }
                continue;
            }
            seen.insert(key, position);
        }
        kept.push((position, identity));
    }
    let panes = kept
        .into_iter()
        .map(|(position, id)| match id {
            Some(id) if conflicts.contains(&id) => Cow::Owned(uncertain(&rows[position], &id)),
            _ => Cow::Borrowed(&rows[position]),
        })
        .collect();
    Index {
        panes,
        conflicts: conflicts.len(),
    }
}
/// Keep existing metadata collections; only pane observation rows are replaced.
pub fn normalize(snapshot: &mut Value) -> usize {
    let Some(rows) = snapshot.get("panes").and_then(Value::as_array) else {
        return 0;
    };
    let result = index(rows);
    let conflicts = result.conflicts;
    let changed = result.panes.len() != rows.len()
        || result
            .panes
            .iter()
            .any(|pane| matches!(pane, Cow::Owned(_)));
    if changed {
        let panes: Vec<_> = result.panes.into_iter().map(Cow::into_owned).collect();
        snapshot["panes"] = json!(panes);
    }
    if conflicts > 0 {
        snapshot["observations_incomplete"] = json!(true);
    } else if let Some(object) = snapshot.as_object_mut() {
        object.remove("observations_incomplete");
    }
    conflicts
}

#[cfg(test)]
mod tests {
    use super::*;
    fn pane(id: &str, tab: &str) -> Value {
        json!({"backend":"herdr","server":"fixture","session":"fixture","workspace_id":"work","tab_id":tab,"pane_id":id,"terminal_id":format!("generation-{id}"),"pid":42,"agent":"codex","agent_session":{"kind":"id","value":"session"},"width":80,"height":24})
    }
    #[test]
    fn duplicates_borrow_once_and_conflicts_propagate_to_aliases_without_tainting_other_terminals()
    {
        let first = pane("one", "first");
        let rows = [first.clone(), first.clone()];
        let clean = index(&rows);
        assert_eq!(clean.panes.len(), 1);
        assert_eq!(clean.conflicts, 0);
        assert!(matches!(clean.panes[0], Cow::Borrowed(_)));
        let mut conflicting = first.clone();
        conflicting["agent_session"]["value"] = json!("different-session");
        let alias = pane("one", "second");
        let other = pane("two", "first");
        for rows in [
            vec![
                first.clone(),
                conflicting.clone(),
                alias.clone(),
                other.clone(),
            ],
            vec![
                conflicting.clone(),
                first.clone(),
                alias.clone(),
                other.clone(),
            ],
        ] {
            let normalized = index(&rows);
            assert_eq!(normalized.conflicts, 1);
            assert_eq!(normalized.panes.len(), 3);
            for pane in &normalized.panes {
                let identity = super::super::terminals::identity(pane).unwrap();
                if identity.pane == "one" {
                    assert!(conflicted(pane));
                    assert!(pane.get("pid").is_none());
                    assert!(pane.get("agent_session").is_none());
                    assert!(pane.get("width").is_none());
                } else {
                    assert_eq!(pane.as_ref(), &other);
                }
            }
        }
    }
    #[test]
    fn normalization_is_idempotent_and_a_later_clean_snapshot_recovers() {
        let first = pane("one", "first");
        let mut conflict = first.clone();
        conflict["width"] = json!(81);
        let mut snapshot = json!({"panes":[first.clone(),conflict],"clients":[{"client":"kept"}]});
        assert_eq!(normalize(&mut snapshot), 1);
        let once = snapshot.clone();
        assert_eq!(normalize(&mut snapshot), 1);
        assert_eq!(snapshot, once);
        snapshot["panes"] = json!([first.clone(), first.clone()]);
        assert_eq!(normalize(&mut snapshot), 0);
        assert_ne!(snapshot["observations_incomplete"], true);
        let mut clean = json!({"panes":[first.clone(),first],"clients":snapshot["clients"]});
        assert_eq!(normalize(&mut clean), 0);
        assert_eq!(clean["panes"].as_array().unwrap().len(), 1);
        assert_ne!(clean["observations_incomplete"], true);
        assert_eq!(clean["clients"], snapshot["clients"]);
    }
}
