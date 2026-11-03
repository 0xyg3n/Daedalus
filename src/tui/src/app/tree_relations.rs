//! Borrowed relationship indexes and bounded optional display metadata.
use super::{discovery_budget::json_size, runtime_settings::Settings};
use crate::core::{identity::TerminalId, routes::Route};
use serde::Serialize;
use serde_json::Value;
use std::{
    collections::{HashMap, HashSet},
    time::{Duration, Instant},
};

pub const WARNING: &str = "Relationship display is incomplete; review saved viewer identities, Relationship detail limits and projection deadline in Terminals and discovery. Omitted details do not prove absence.";

pub struct Budget {
    entries: usize,
    bytes: usize,
    deadline: Instant,
    pub limited: bool,
}
impl Budget {
    pub fn new(settings: &Settings) -> Self {
        let now = Instant::now();
        let valid = settings.validate().is_ok();
        Self {
            entries: if valid {
                settings.discovery_relationship_limit as usize
            } else {
                0
            },
            bytes: if valid {
                settings.discovery_relationship_limit_bytes as usize
            } else {
                0
            },
            deadline: now
                .checked_add(Duration::from_millis(
                    settings.discovery_projection_timeout_ms,
                ))
                .unwrap_or(now),
            limited: !valid,
        }
    }
    pub fn time_left(&mut self) -> bool {
        if Instant::now() >= self.deadline {
            self.limited = true;
            false
        } else {
            true
        }
    }
    pub fn admit(&mut self, value: &impl Serialize) -> bool {
        if !self.time_left() {
            return false;
        }
        if self.entries == 0 {
            self.limited = true;
            return false;
        }
        let Ok(bytes) = json_size(value, self.bytes) else {
            self.limited = true;
            return false;
        };
        if !self.time_left() {
            return false;
        }
        self.entries -= 1;
        self.bytes -= bytes;
        true
    }
    pub fn details_left(&mut self) -> bool {
        if !self.time_left() {
            return false;
        }
        if self.entries == 0 || self.bytes == 0 {
            self.limited = true;
            false
        } else {
            true
        }
    }
}
fn rows<'a>(value: &'a Value, key: &str) -> &'a [Value] {
    value
        .get(key)
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[])
}
fn field<'a>(value: &'a Value, key: &str) -> &'a str {
    value.get(key).and_then(Value::as_str).unwrap_or("")
}
#[derive(Default)]
pub struct Clients<'a> {
    pub regular: usize,
    pub control: usize,
    pub active: HashSet<&'a str>,
    pub typing: HashMap<&'a str, usize>,
}
#[derive(Default)]
pub struct Index<'a> {
    pub clients: HashMap<(&'a str, &'a str), Clients<'a>>,
    pub clients_complete: bool,
    pub views_complete: bool,
    pub nested: HashMap<(&'a str, &'a str), Vec<&'a Value>>,
    pub viewers: HashMap<TerminalId, &'a Value>,
    pub sources: HashMap<TerminalId, Vec<&'a Value>>,
    pub checkpoints: HashMap<(&'a str, &'a str), Vec<&'a Value>>,
    pub routes: HashMap<TerminalId, Vec<&'a str>>,
    pub focus: HashSet<(&'a str, &'a str)>,
    pub topology: HashMap<(&'a str, &'a str, &'a Value), &'a Value>,
}
impl<'a> Index<'a> {
    pub fn new(
        snapshot: &'a Value,
        health: &'a Value,
        budget: &mut Budget,
        include_checkpoints: bool,
    ) -> Self {
        let mut index = Self {
            clients_complete: true,
            views_complete: true,
            ..Self::default()
        };
        for client in rows(snapshot, "clients") {
            if !budget.time_left() {
                index.clients_complete = false;
                break;
            }
            if field(client, "backend") != "tmux" {
                continue;
            }
            let stats = index
                .clients
                .entry((field(client, "socket"), field(client, "session_id")))
                .or_default();
            let pane = field(client, "pane_id");
            stats.active.insert(pane);
            let flags = field(client, "flags");
            if flags.split(',').any(|flag| flag == "control-mode") {
                stats.control += 1;
            } else {
                stats.regular += 1;
                if !flags.split(',').any(|flag| flag == "read-only") {
                    *stats.typing.entry(pane).or_default() += 1;
                }
            }
            for outer in rows(client, "outer_panes") {
                if !budget.time_left() {
                    break;
                }
                index
                    .nested
                    .entry((field(outer, "socket"), field(outer, "pane_id")))
                    .or_default()
                    .push(client);
            }
        }
        let views =
            super::view_records::resolve(rows(snapshot, "daedalus_views"), || budget.time_left());
        index.views_complete = views.unresolved == 0 && snapshot["view_records_incomplete"] != true;
        if !index.views_complete {
            budget.limited = true;
        }
        for view in views.records {
            index.viewers.insert(view.viewer, view.row);
            index.sources.entry(view.source).or_default().push(view.row);
        }
        if include_checkpoints {
            for saved in rows(health, "checkpoints") {
                if !budget.time_left() {
                    break;
                }
                index
                    .checkpoints
                    .entry((field(saved, "harness"), field(saved, "sid")))
                    .or_default()
                    .push(saved);
            }
        }
        for saved in rows(health, "native_routes") {
            if !budget.time_left() {
                break;
            }
            if let Ok(route) = serde_json::from_value::<Route>(saved.clone())
                && route.revision > 0
                && route.scope.validate().is_ok()
                && route.terminal.validate().is_ok()
            {
                index
                    .routes
                    .entry(route.terminal)
                    .or_default()
                    .push(field(&saved["scope"], "chat"));
            }
        }
        for focus in rows(snapshot, "herdr_focus") {
            if !budget.time_left() {
                break;
            }
            index
                .focus
                .insert((field(focus, "session"), field(focus, "pane_id")));
        }
        for (collection, key) in [("workspaces", "workspace_id"), ("tabs", "tab_id")] {
            for item in rows(snapshot, collection) {
                if !budget.time_left() {
                    break;
                }
                if let Some(id) = item.get(key) {
                    index
                        .topology
                        .entry((collection, field(item, "session"), id))
                        .or_insert(item);
                }
            }
        }
        index
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn admission_preserves_small_later_details_and_refuses_expired_indexes() {
        let settings = Settings {
            discovery_relationship_limit: 2,
            discovery_relationship_limit_bytes: 1024,
            ..Settings::default()
        };
        let mut budget = Budget::new(&settings);
        assert!(!budget.admit(&"x".repeat(1024)));
        assert!(budget.admit(&"small"));
        assert!(budget.admit(&"second"));
        assert!(!budget.admit(&"third"));
        assert!(budget.limited);
        let mut budget = Budget::new(&settings);
        budget.deadline = Instant::now() - Duration::from_millis(1);
        let snapshot = json!({"clients":[{"backend":"tmux","socket":"fixture","session_id":"$1","pane_id":"%1"}]});
        let health = json!({});
        let index = Index::new(&snapshot, &health, &mut budget, true);
        assert!(!index.clients_complete && index.clients.is_empty());
        assert!(budget.limited && !budget.admit(&"late"));
    }
}
