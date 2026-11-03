//! Pure projection of discovered topology into the existing TUI protocol.
//! Display names never identify terminals. Callers supply time/home/hostname;
//! projecting a snapshot performs no filesystem, terminal or network reads.
use super::tree_relations::{Budget as RelationshipBudget, Index as RelationshipIndex};
use serde_json::{Map, Value, json};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone, Default)]
pub struct ProjectionContext {
    pub hostname: String,
    pub home: String,
    pub now: f64,
    pub pins: Vec<Value>,
    pub runtime: super::runtime_settings::Settings,
    pub checkpoint_matches: super::interface_settings::CheckpointMatches,
}
#[derive(Debug, Clone)]
pub struct Node {
    pub key: Vec<Value>,
    pub parent: Option<usize>,
    pub children: Vec<usize>,
    pub label: String,
    pub kind: String,
    pub pane: Option<Value>,
    pub attributes: Map<String, Value>,
    shared_views: Option<std::sync::Arc<Value>>,
}
impl Node {
    pub fn id(&self) -> String {
        key_id(&self.key)
    }
    fn attr_ref(&self, key: &str) -> Option<&Value> {
        if key == "views" {
            self.shared_views
                .as_deref()
                .or_else(|| self.attributes.get(key))
        } else {
            self.attributes.get(key)
        }
    }
    fn attr(&self, key: &str) -> Value {
        self.attr_ref(key).cloned().unwrap_or(Value::Null)
    }
}
#[derive(Debug, Clone, Default)]
pub struct Tree {
    pub nodes: Vec<Node>,
    pub roots: Vec<usize>,
    by_id: BTreeMap<String, usize>,
    pub relationships_limited: bool,
    pub layout_warnings: Vec<&'static str>,
    pub observation_warnings: Vec<&'static str>,
}
fn array<'a>(value: &'a Value, key: &str) -> &'a [Value] {
    value
        .get(key)
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[])
}
fn text(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        Value::Null => "None".into(),
        Value::Bool(true) => "True".into(),
        Value::Bool(false) => "False".into(),
        _ => value.to_string(),
    }
}
fn s(value: &Value, key: &str) -> String {
    value.get(key).map(text).unwrap_or_default()
}
fn truth(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::String(s) => !s.is_empty(),
        Value::Array(a) => !a.is_empty(),
        Value::Object(m) => !m.is_empty(),
        Value::Number(n) => n.as_f64().unwrap_or_default() != 0.0,
    }
}
fn first(value: &Value, fields: &[&str]) -> String {
    fields
        .iter()
        .filter_map(|key| value.get(*key))
        .find(|v| truth(v))
        .map(text)
        .unwrap_or_default()
}
fn v(value: &Value, key: &str) -> Value {
    value.get(key).cloned().unwrap_or(Value::Null)
}
fn extend(key: &[Value], parts: impl IntoIterator<Item = Value>) -> Vec<Value> {
    let mut result = key.to_vec();
    result.extend(parts);
    result
}
fn lower(value: &str) -> String {
    value.to_lowercase().replace('ß', "ss").replace('ς', "σ")
}
const HARNESSES: &[&str] = &[
    "claude", "codex", "grok", "hermes", "cursor", "pi", "opencode", "agy", "gateway",
];
const SHELLS: &[&str] = &["bash", "zsh", "fish", "sh"];
pub fn is_agent(pane: &Value) -> bool {
    let agent = lower(&first(pane, &["agent"]));
    if s(pane, "backend") == "herdr" {
        !agent.is_empty() && !SHELLS.contains(&agent.as_str())
    } else {
        HARNESSES.contains(&agent.as_str())
            || crate::adapters::harnesses::canonical_id(&agent).is_some()
    }
}
fn graphics_title(title: &str) -> bool {
    let prefix = title.split(';').next().unwrap_or(title);
    let Some(prefix) = prefix.strip_prefix('G') else {
        return false;
    };
    prefix.split(',').all(|part| {
        let Some((name, value)) = part.split_once('=') else {
            return false;
        };
        name.len() == 1
            && name.bytes().all(|b| b.is_ascii_alphabetic())
            && value
                .chars()
                .all(|c| c.is_alphanumeric() || c == '_' || c == '-')
    })
}
pub fn title(pane: &Value, hostname: &str) -> String {
    let mut explicit = first(pane, &["title"]);
    if explicit == hostname
        || explicit == hostname.split('.').next().unwrap_or(hostname)
        || graphics_title(&explicit)
    {
        explicit.clear();
    }
    let label = first(pane, &["label"]);
    if !label.is_empty() {
        label
    } else if !explicit.is_empty() {
        explicit
    } else {
        let agent = first(pane, &["agent"]);
        if agent.is_empty() {
            "Terminal".into()
        } else {
            agent
        }
    }
}
fn label_parts(pane: &Value, title: &str, hostname: &str) -> Value {
    let pieces: Vec<_> = title
        .split(" · ")
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .collect();
    let agent = lower(&first(pane, &["agent"]));
    let harness = pieces
        .iter()
        .position(|p| lower(p) == agent || HARNESSES.contains(&lower(p).as_str()));
    let mut parts = vec![json!([s(pane, "pane_id"), "id"])];
    for (i, piece) in pieces.iter().enumerate() {
        let role = match harness {
            Some(h) if i == h => "harness",
            Some(h) if i > h => "model",
            _ => "title",
        };
        parts.push(json!([piece, role]));
    }
    let topic = first(pane, &["terminal_title_stripped", "terminal_title"]);
    let topic = topic.trim();
    let folded = lower(topic);
    let mut known: BTreeSet<String> = pieces.iter().map(|p| lower(p)).collect();
    known.extend(SHELLS.iter().map(|s| (*s).into()));
    known.insert(lower(hostname));
    known.insert(lower(hostname.split('.').next().unwrap_or(hostname)));
    if !topic.is_empty() && !known.contains(&folded) {
        parts.push(json!([topic, "topic"]));
    }
    parts.into()
}
fn native_sid(pane: &Value) -> String {
    crate::adapters::terminals::reported_session(pane)
        .unwrap_or_default()
        .into()
}
fn session_gap(pane: &Value, health: &Value) -> Value {
    if s(pane, "backend") == "native" {
        return json!([
            "",
            "not reported · session hooks not yet verified",
            "Open Hooks to connect this harness's verified conversation; saving a checkpoint can also accept its explicit session ID."
        ]);
    }
    if s(pane, "backend") != "herdr" {
        return json!(["", "not reported · tmux reports none", ""]);
    }
    let tool = s(pane, "agent");
    let found = array(&health["hooks"], "tools")
        .iter()
        .find(|t| s(t, "tool") == tool)
        .map(|t| &t["herdr"])
        .unwrap_or(&Value::Null);
    match s(found, "state").as_str() {
        "current" => json!(["", "not reported yet by Herdr", ""]),
        state @ ("outdated" | "absent") => {
            let brief = if state == "outdated" {
                "Herdr hook outdated"
            } else {
                "Herdr hook not installed"
            };
            let note = first(found, &["note"]);
            let full = if state == "outdated" && !note.is_empty() {
                format!("{brief} ({note})")
            } else {
                brief.into()
            };
            let name = match tool.as_str() {
                "claude" => "Claude Code",
                "codex" => "Codex",
                "grok" => "Grok",
                _ => tool.as_str(),
            };
            json!([
                brief,
                format!("not reported · {full}"),
                format!(
                    "Hooks (6) → {name} → {} Herdr hook",
                    if state == "outdated" {
                        "Update"
                    } else {
                        "Install"
                    }
                )
            ])
        }
        _ => json!(["", "not reported", ""]),
    }
}
/// Python json.dumps tuple formatting is a persisted protocol identifier.
/// Preserve spaces and ASCII escapes, including UTF-16 surrogate pairs.
pub fn key_id(key: &[Value]) -> String {
    fn encode(value: &Value) -> String {
        match value {
            Value::String(value) => {
                let mut out = String::from("\"");
                for ch in value.chars() {
                    match ch {
                        '"' => out.push_str("\\\""),
                        '\\' => out.push_str("\\\\"),
                        '\n' => out.push_str("\\n"),
                        '\r' => out.push_str("\\r"),
                        '\t' => out.push_str("\\t"),
                        '\u{8}' => out.push_str("\\b"),
                        '\u{c}' => out.push_str("\\f"),
                        c if !(' '..='~').contains(&c) => {
                            let mut units = [0u16; 2];
                            for unit in c.encode_utf16(&mut units) {
                                out.push_str(&format!("\\u{unit:04x}"));
                            }
                        }
                        c => out.push(c),
                    }
                }
                out.push('"');
                out
            }
            Value::Array(values) => format!(
                "[{}]",
                values.iter().map(encode).collect::<Vec<_>>().join(", ")
            ),
            _ => value.to_string(),
        }
    }
    encode(&Value::Array(key.to_vec()))
}
impl Tree {
    fn add(
        &mut self,
        key: Vec<Value>,
        parent: Option<usize>,
        label: impl Into<String>,
        kind: &str,
        pane: Option<&Value>,
    ) -> usize {
        let id = key_id(&key);
        if let Some(index) = self.by_id.get(&id) {
            return *index;
        }
        let index = self.nodes.len();
        self.nodes.push(Node {
            key,
            parent,
            children: Vec::new(),
            label: label.into(),
            kind: kind.into(),
            pane: pane.cloned(),
            attributes: Map::new(),
            shared_views: None,
        });
        self.by_id.insert(id, index);
        if let Some(parent) = parent {
            self.nodes[parent].children.push(index);
        } else {
            self.roots.push(index);
        }
        index
    }
    fn add_lazy(
        &mut self,
        key: Vec<Value>,
        parent: Option<usize>,
        label: impl FnOnce() -> String,
        kind: &str,
    ) -> usize {
        if let Some(index) = self.by_id.get(&key_id(&key)) {
            return *index;
        }
        self.add(key, parent, label(), kind, None)
    }
    pub fn find(&self, id: &str) -> Option<&Node> {
        self.by_id.get(id).map(|i| &self.nodes[*i])
    }
    fn add_relationship(
        &mut self,
        budget: &mut RelationshipBudget,
        key: Vec<Value>,
        parent: usize,
        label: String,
        kind: &str,
        attribute: Option<(&str, &Value)>,
    ) -> bool {
        if self.by_id.contains_key(&key_id(&key)) {
            return true;
        }
        if !budget.admit(&(&key, &label, kind, attribute)) {
            return false;
        }
        let node = self.add(key, Some(parent), label, kind, None);
        if let Some((key, value)) = attribute {
            self.nodes[node]
                .attributes
                .insert(key.into(), value.clone());
        }
        true
    }
    fn add_checkpoint(
        &mut self,
        budget: &mut RelationshipBudget,
        parent: usize,
        saved: &Value,
        pane: &Value,
        mode: super::interface_settings::CheckpointMatches,
    ) -> bool {
        let key = extend(
            &self.nodes[parent].key,
            ["checkpoint".into(), v(saved, "key")],
        );
        if self.by_id.contains_key(&key_id(&key)) {
            return true;
        }
        let origin = saved
            .get("source")
            .and_then(crate::core::layouts::TerminalRef::decode_identity)
            .zip(crate::adapters::terminals::reference(pane).ok())
            .is_some_and(|(a, b)| a.same_identity(&b));
        if mode == super::interface_settings::CheckpointMatches::Off
            || (mode == super::interface_settings::CheckpointMatches::Source && !origin)
        {
            return true;
        }
        let prefix = if origin {
            "Checkpoint saved here"
        } else {
            "Checkpoint ID match"
        };
        let label = format!("{prefix} · {}", s(saved, "name"));
        let evidence = json!({"kind":if origin{"source_origin"}else{"native_id_only"},"message":if origin{"Saved from this terminal; current profile is unverified."}else{"Native ID match only; current profile and source affiliation are unverified."}});
        let parts = json!([
            [prefix, "rose"],
            [s(saved, "name"), "title"],
            ["Profile unverified", "warn"]
        ]);
        if !budget.admit(&(
            &key,
            &label,
            "checkpoint",
            ("checkpoint", saved),
            ("checkpoint_match", &evidence),
            ("parts", &parts),
        )) {
            return false;
        }
        let node = self.add(key, Some(parent), label, "checkpoint", None);
        self.nodes[node]
            .attributes
            .insert("checkpoint".into(), saved.clone());
        self.nodes[node]
            .attributes
            .insert("checkpoint_match".into(), evidence);
        self.nodes[node].attributes.insert("parts".into(), parts);
        true
    }
    pub fn build(snapshot: &Value, health: &Value, context: &ProjectionContext) -> Self {
        let mut tree = Self::default();
        let mut relationship_budget = RelationshipBudget::new(&context.runtime);
        let mut relationships = RelationshipIndex::new(
            snapshot,
            health,
            &mut relationship_budget,
            context.checkpoint_matches != super::interface_settings::CheckpointMatches::Off,
        );
        let observations = crate::adapters::pane_observations::index(array(snapshot, "panes"));
        if observations.conflicts > 0 {
            tree.observation_warnings
                .push(crate::adapters::pane_observations::WARNING);
            relationships.views_complete = false;
        }
        let admitted: Vec<_> = observations
            .panes
            .iter()
            .map(|pane| pane.as_ref())
            .collect();
        let view_index = view_index(&admitted);
        let mut view_cache = std::collections::HashMap::new();
        let mut panes: Vec<&Value> = admitted;
        panes.sort_by_key(|p| {
            [
                "backend",
                "socket",
                "session",
                "workspace_id",
                "tab_id",
                "window",
                "pane_id",
            ]
            .map(|k| s(p, k))
        });
        let mut groups: BTreeMap<(String, String), BTreeMap<String, String>> = BTreeMap::new();
        let mut kept = std::collections::HashMap::new();
        for (index, pane) in panes.iter().enumerate() {
            if s(pane, "backend") == "tmux" && !first(pane, &["session_group"]).is_empty() {
                groups
                    .entry((s(pane, "socket"), s(pane, "session_group")))
                    .or_default()
                    .insert(s(pane, "session_id"), s(pane, "session"));
                if let Ok(identity) = crate::adapters::terminals::identity(pane) {
                    let selected = kept
                        .entry((identity, s(pane, "session_group")))
                        .or_insert(index);
                    if s(pane, "session") == s(pane, "session_group")
                        && s(panes[*selected], "session") != s(pane, "session")
                    {
                        *selected = index;
                    }
                }
            }
        }
        panes = panes
            .iter()
            .enumerate()
            .filter(|(i, p)| {
                s(p, "backend") != "tmux"
                    || first(p, &["session_group"]).is_empty()
                    || crate::adapters::terminals::identity(p)
                        .map_or(true, |id| kept.get(&(id, s(p, "session_group"))) == Some(i))
            })
            .map(|(_, p)| *p)
            .collect();
        let mut identities = std::collections::HashMap::new();
        for pane in &panes {
            if let Ok(id) = crate::adapters::terminals::identity(pane) {
                *identities.entry(id).or_insert(0usize) += 1;
            }
        }
        for pane in panes {
            let backend = s(pane, "backend");
            let server = first(pane, &["socket", "server", "session"]);
            let herdr = backend == "herdr";
            let native = backend == "native";
            let root_key = vec![backend.clone().into(), server.clone().into()];
            let root = tree.add_lazy(
                root_key.clone(),
                None,
                || {
                    if native {
                        "DAEDALUS · Native terminals".into()
                    } else {
                        format!("{} · {server}", backend.to_uppercase())
                    }
                },
                "server",
            );
            let project = if herdr {
                v(pane, "workspace_id")
            } else {
                let id = first(pane, &["session_id", "session"]);
                Value::String(id)
            };
            let project_key = extend(&root_key, ["project".into(), project.clone()]);
            let sessions = groups.get(&(s(pane, "socket"), s(pane, "session_group")));
            let project_label = || {
                if native {
                    first(pane, &["workspace_label", "workspace_id"])
                } else if herdr {
                    topology_label(
                        &relationships,
                        "workspaces",
                        &server,
                        &project,
                        &text(&project),
                    )
                } else {
                    format!(
                        "{}{}",
                        s(pane, "session"),
                        sessions
                            .filter(|g| g.len() > 1)
                            .map(|g| format!(" (+{} grouped)", g.len() - 1))
                            .unwrap_or_default()
                    )
                }
            };
            let project_node = tree.add_lazy(
                project_key.clone(),
                Some(root),
                project_label,
                if herdr || native {
                    "workspace"
                } else {
                    "tmux session"
                },
            );
            let group = if herdr {
                v(pane, "tab_id")
            } else {
                let id = first(pane, &["window_id", "window"]);
                Value::String(if id.is_empty() { "?".into() } else { id })
            };
            let group_key = extend(&project_key, ["group".into(), group.clone()]);
            let group_label = || {
                if herdr {
                    topology_label(&relationships, "tabs", &server, &group, &text(&group))
                } else {
                    pane.get("window").map(text).unwrap_or_else(|| text(&group))
                }
            };
            let group_node = tree.add_lazy(
                group_key.clone(),
                Some(project_node),
                group_label,
                if herdr { "tab" } else { "window" },
            );
            let pane_key = extend(
                &group_key,
                [
                    "pane".into(),
                    v(pane, "pane_id"),
                    pane.get("terminal_id").cloned().unwrap_or("".into()),
                ],
            );
            let viewer = (!crate::adapters::pane_observations::conflicted(pane))
                .then(|| crate::adapters::terminals::identity(pane).ok())
                .flatten()
                .and_then(|identity| relationships.viewers.get(&identity))
                .copied();
            let title = viewer
                .map(|v| {
                    format!(
                        "View of {}",
                        v.get("source_label").map(text).unwrap_or("terminal".into())
                    )
                })
                .unwrap_or_else(|| title(pane, &context.hostname));
            let state = {
                let state = first(pane, &["agent_status"]);
                if !state.is_empty() {
                    state
                } else if ["bash", "zsh", "fish"].contains(&s(pane, "agent").as_str()) {
                    "shell".into()
                } else {
                    "process present".into()
                }
            };
            let pane_node = tree.add(
                pane_key.clone(),
                Some(group_node),
                if native {
                    title.clone()
                } else {
                    format!("{} · {title}", s(pane, "pane_id"))
                },
                "pane",
                Some(pane),
            );
            let active_at = pane.get("activity_at").and_then(number).unwrap_or_default();
            let activity = first(pane, &["activity"]);
            let linked = crate::adapters::terminals::identity(pane)
                .ok()
                .and_then(|id| identities.get(&id))
                .copied()
                .unwrap_or(1);
            let viewed = backend == "tmux"
                && sessions
                    .map(|group| {
                        group.keys().any(|id| {
                            relationships
                                .clients
                                .get(&(s(pane, "socket").as_str(), id.as_str()))
                                .is_some_and(|stats| {
                                    stats.active.contains(s(pane, "pane_id").as_str())
                                })
                        })
                    })
                    .unwrap_or_else(|| {
                        relationships
                            .clients
                            .get(&(s(pane, "socket").as_str(), s(pane, "session_id").as_str()))
                            .is_some_and(|stats| stats.active.contains(s(pane, "pane_id").as_str()))
                    });
            let focused = herdr
                && relationships
                    .focus
                    .contains(&(s(pane, "session").as_str(), s(pane, "pane_id").as_str()));
            let shared = if let Ok(id) = crate::adapters::terminals::identity(pane) {
                view_cache
                    .entry(id)
                    .or_insert_with(|| {
                        std::sync::Arc::new(views(
                            pane,
                            &view_index,
                            &relationships,
                            &mut relationship_budget,
                        ))
                    })
                    .clone()
            } else {
                std::sync::Arc::new(views(
                    pane,
                    &view_index,
                    &relationships,
                    &mut relationship_budget,
                ))
            };
            tree.nodes[pane_node].shared_views = Some(shared);
            tree.nodes[pane_node].attributes=json!({"parts":label_parts(pane,&title,&context.hostname),"viewer":viewer,"state":state,"activity_at":active_at,"activity":activity,"linked":linked,"viewed":viewed,"server_focused":focused}).as_object().unwrap().clone();
            if let Some(key) = super::workspace_usage::key(pane)
                && let Some(usage) = snapshot.get("workspace_usage").and_then(|u| u.get(&key))
            {
                tree.nodes[pane_node]
                    .attributes
                    .insert("usage".into(), usage.clone());
            }
            if backend == "tmux" {
                for client in relationships
                    .nested
                    .get(&(s(pane, "socket").as_str(), s(pane, "pane_id").as_str()))
                    .into_iter()
                    .flatten()
                {
                    if !relationship_budget.details_left() {
                        break;
                    }
                    let key = extend(
                        &pane_key,
                        [
                            "nested".into(),
                            s(client, "socket").into(),
                            s(client, "client").into(),
                        ],
                    );
                    if !tree.add_relationship(
                        &mut relationship_budget,
                        key,
                        pane_node,
                        format!(
                            "Nested tmux view → {} / {}",
                            s(client, "socket"),
                            s(client, "session_id")
                        ),
                        "nested view",
                        None,
                    ) {
                        break;
                    }
                }
            }
            if backend == "tmux" && !first(pane, &["herdr_session"]).is_empty() {
                tree.add_relationship(
                    &mut relationship_budget,
                    extend(&pane_key, ["herdr-view".into(), v(pane, "herdr_session")]),
                    pane_node,
                    format!("Herdr view → {}", s(pane, "herdr_session")),
                    "declared view",
                    None,
                );
            }
            let sid = native_sid(pane);
            let agent_here = is_agent(pane);
            if agent_here && sid.is_empty() {
                tree.nodes[pane_node]
                    .attributes
                    .insert("sid_gap".into(), session_gap(pane, health));
            }
            if agent_here || !sid.is_empty() {
                let key = extend(
                    &pane_key,
                    [
                        "agent".into(),
                        pane.get("agent").cloned().unwrap_or("unknown".into()),
                        sid.clone().into(),
                    ],
                );
                let harness = {
                    let h = first(pane, &["agent"]);
                    if h.is_empty() { "Agent".into() } else { h }
                };
                let short = sid.chars().take(12).collect::<String>();
                let agent_node = tree.add(
                    key.clone(),
                    Some(pane_node),
                    if sid.is_empty() {
                        harness.clone()
                    } else {
                        format!("{harness} · {short}")
                    },
                    "agent session",
                    Some(pane),
                );
                let gap = tree.nodes[pane_node].attr("sid_gap");
                let brief = gap.get(0).and_then(Value::as_str).unwrap_or_default();
                let mut parts = vec![json!([harness, "harness"])];
                if !sid.is_empty() {
                    parts.push(json!([short, "sid"]));
                }
                if !brief.is_empty() {
                    parts.push(json!([brief, "warn"]));
                }
                tree.nodes[agent_node].attributes=json!({"sid":sid,"sid_gap":gap,"parts":parts,"state":state,"activity_at":active_at,"activity":activity,"linked":linked}).as_object().unwrap().clone();
                if !sid.is_empty() {
                    for saved in relationships
                        .checkpoints
                        .get(&(s(pane, "agent").as_str(), sid.as_str()))
                        .into_iter()
                        .flatten()
                    {
                        if !relationship_budget.details_left() {
                            break;
                        }
                        if !tree.add_checkpoint(
                            &mut relationship_budget,
                            agent_node,
                            saved,
                            pane,
                            context.checkpoint_matches,
                        ) {
                            break;
                        }
                    }
                }
            }
            // Native routes carry the complete identity; labels and pane numbers
            // alone must never attach a route to a replacement terminal.
            if let Ok(identity) = crate::adapters::terminals::identity(pane) {
                for chat in relationships.routes.get(&identity).into_iter().flatten() {
                    if !relationship_budget.details_left() {
                        break;
                    }
                    if !tree.add_relationship(
                        &mut relationship_budget,
                        extend(&pane_key, ["telegram".into(), (*chat).into()]),
                        pane_node,
                        format!("Telegram chat · {chat}"),
                        "saved route",
                        None,
                    ) {
                        break;
                    }
                }
            }
        }
        let projected = super::tree_layout::project(&tree, snapshot, &context.runtime);
        tree.layout_warnings = projected.warnings.into_iter().collect();
        for group in projected.groups {
            tree.nodes[group.parent]
                .attributes
                .insert("layout".into(), group.value);
            let expected: std::collections::HashSet<_> =
                tree.nodes[group.parent].children.iter().copied().collect();
            let ordered: std::collections::HashSet<_> = group.order.iter().copied().collect();
            let mut children: Vec<_> = group
                .order
                .iter()
                .copied()
                .filter(|index| expected.contains(index))
                .collect();
            children.extend(
                tree.nodes[group.parent]
                    .children
                    .iter()
                    .copied()
                    .filter(|index| !ordered.contains(index)),
            );
            tree.nodes[group.parent].children = children;
            for index in &group.order {
                tree.nodes[*index]
                    .attributes
                    .insert("split".into(), group.order.len().into());
            }
        }
        for (collection, kind) in [("workspaces", "workspace"), ("tabs", "tab")] {
            for item in array(snapshot, collection) {
                let server = first(item, &["session"]);
                if server.is_empty() {
                    continue;
                }
                let root_key = vec!["herdr".into(), server.clone().into()];
                let root = tree.add(
                    root_key.clone(),
                    None,
                    format!("HERDR · {server}"),
                    "server",
                    None,
                );
                let project = v(item, "workspace_id");
                if !truth(&project) {
                    continue;
                }
                let key = extend(&root_key, ["project".into(), project.clone()]);
                let project_node = tree.add(
                    key.clone(),
                    Some(root),
                    topology_label(
                        &relationships,
                        "workspaces",
                        &server,
                        &project,
                        &text(&project),
                    ),
                    "workspace",
                    None,
                );
                if kind == "tab" && truth(&item["tab_id"]) {
                    tree.add(
                        extend(&key, ["group".into(), v(item, "tab_id")]),
                        Some(project_node),
                        first(item, &["label", "tab_id"]),
                        "tab",
                        None,
                    );
                }
            }
        }
        let external = array(snapshot, "external_terminals");
        let external_errors = array(snapshot, "external_errors");
        if !external.is_empty() || !external_errors.is_empty() {
            let root_key = vec![json!("external")];
            let root = tree.add(
                root_key.clone(),
                None,
                "External terminals",
                "external group",
                None,
            );
            tree.nodes[root]
                .attributes
                .insert("external_count".into(), external.len().into());
            tree.nodes[root]
                .attributes
                .insert("errors".into(), external_errors.to_vec().into());
            for process in external {
                let id = first(process, &["id"]);
                if id.is_empty() {
                    continue;
                }
                let name = first(process, &["name"]);
                let pid = first(process, &["pid"]);
                let node = tree.add(
                    extend(&root_key, [id.into()]),
                    Some(root),
                    format!("{name} · PID {pid}"),
                    "external",
                    None,
                );
                tree.nodes[node]
                    .attributes
                    .insert("process".into(), process.clone());
                tree.nodes[node]
                    .attributes
                    .insert("state".into(), v(process, "state"));
                tree.nodes[node].attributes.insert(
                    "parts".into(),
                    json!([
                        [name, "harness"],
                        [format!("PID {pid}"), "id"],
                        ["read only", "muted"]
                    ]),
                );
            }
        }
        // Reverse insertion order is a postorder because parents precede children.
        for index in (0..tree.nodes.len()).rev() {
            let count = usize::from(tree.nodes[index].kind == "pane")
                + tree.nodes[index]
                    .children
                    .iter()
                    .map(|i| {
                        tree.nodes[*i]
                            .attributes
                            .get("pane_count")
                            .and_then(Value::as_u64)
                            .unwrap_or_default() as usize
                    })
                    .sum::<usize>();
            tree.nodes[index]
                .attributes
                .insert("pane_count".into(), count.into());
        }
        tree.relationships_limited = relationship_budget.limited;
        tree
    }
}
fn topology_label(
    index: &RelationshipIndex<'_>,
    collection: &str,
    server: &str,
    id: &Value,
    fallback: &str,
) -> String {
    index
        .topology
        .get(&(collection, server, id))
        .map(|row| {
            let name = first(row, &["label", "name"]);
            if name.is_empty() {
                fallback.into()
            } else {
                name
            }
        })
        .unwrap_or_else(|| fallback.into())
}
fn number(value: &Value) -> Option<f64> {
    value
        .as_f64()
        .or_else(|| value.as_str().and_then(|s| s.parse().ok()))
        .filter(|n| n.is_finite())
}
type ViewIndex<'a> = std::collections::HashMap<crate::core::identity::TerminalId, Vec<&'a Value>>;
fn view_index<'a>(panes: &[&'a Value]) -> ViewIndex<'a> {
    let mut index = ViewIndex::new();
    for pane in panes {
        if let Ok(id) = crate::adapters::terminals::identity(pane) {
            index.entry(id).or_default().push(*pane);
        }
    }
    index
}
fn views(
    pane: &Value,
    index: &ViewIndex<'_>,
    relationships: &RelationshipIndex<'_>,
    budget: &mut RelationshipBudget,
) -> Value {
    let id = crate::adapters::terminals::identity(pane).ok();
    let fallback = [pane];
    let same = id
        .as_ref()
        .and_then(|id| index.get(id))
        .map(Vec::as_slice)
        .unwrap_or(&fallback);
    let viewers = id
        .as_ref()
        .and_then(|identity| relationships.sources.get(identity))
        .map(Vec::as_slice)
        .unwrap_or(&[]);
    let count = same.len() + viewers.len();
    let mut places = Vec::new();
    for p in same {
        if !budget.details_left() {
            break;
        }
        let session = first(p, &["session"]);
        let backend = s(p, "backend");
        let kind = if session.starts_with("daedalus-view-") {
            "Daedalus view session"
        } else if backend == "native" {
            "Native workspace"
        } else if backend == "tmux" {
            "tmux session"
        } else {
            "Herdr workspace"
        };
        let place = if backend == "native" {
            format!(
                "{} · {}",
                first(p, &["workspace_label", "workspace_id"]),
                first(p, &["title", "window"])
            )
        } else if backend == "herdr" {
            format!(
                "{} · {}",
                first(p, &["workspace_id", "session"]),
                first(p, &["tab_id"])
            )
        } else {
            let window = first(p, &["window_index", "window"]);
            format!(
                "{session} · window {}",
                if window.is_empty() { "?" } else { &window }
            )
        };
        let place = format!("{kind} {place}");
        if budget.admit(&place) {
            places.push(place);
        }
    }
    for viewer in viewers {
        if !budget.details_left() {
            break;
        }
        let place = format!("Daedalus viewer pane {}", s(viewer, "label"));
        if budget.admit(&place) {
            places.push(place);
        }
    }
    // Only aggregate counts are consumed by the renderer. Retaining a full copy
    // of every client label for every terminal produces a quadratic expansion.
    let mut typing = 0usize;
    let mut regular = 0usize;
    let mut control = 0usize;
    if s(pane, "backend") == "tmux" {
        let socket = s(pane, "socket");
        let active = s(pane, "pane_id");
        let mut sessions = std::collections::HashSet::new();
        for p in same {
            let session = p.get("session_id").and_then(Value::as_str).unwrap_or("");
            if sessions.insert(session)
                && let Some(stats) = relationships.clients.get(&(socket.as_str(), session))
            {
                regular += stats.regular;
                control += stats.control;
                typing += stats
                    .typing
                    .get(active.as_str())
                    .copied()
                    .unwrap_or_default();
            }
        }
    }
    let mut result = json!({"count":count,"details_limited":places.len()<count || !relationships.views_complete,"places":places,"typing":typing,"watching":regular.saturating_sub(typing),"control_clients":control,"clients_complete":relationships.clients_complete,"shared":s(pane,"backend")=="tmux"});
    if s(pane, "backend") == "native" {
        result["controlled"] = pane["controlled"].clone();
    }
    result
}
fn input_brief(info: &Value, backend: &str) -> [String; 2] {
    if backend == "native" {
        return if info["controlled"] == true {
            [
                "One controller attached · Watch is available".into(),
                "info".into(),
            ]
        } else {
            [
                "Input available · one controller at a time".into(),
                "ok".into(),
            ]
        };
    }
    if backend == "herdr" {
        return [
            "One attach slot · asks before takeover".into(),
            "muted".into(),
        ];
    }
    if info["clients_complete"] == false {
        return ["Shared · client activity unavailable".into(), "warn".into()];
    }
    let typing = info["typing"].as_u64().unwrap_or_default();
    let mut parts = Vec::new();
    for (count, name) in [
        (typing, "typing"),
        (info["watching"].as_u64().unwrap_or_default(), "watching"),
        (
            info["control_clients"].as_u64().unwrap_or_default(),
            "control-mode",
        ),
    ] {
        if count > 0 {
            parts.push(format!("{count} {name}"));
        }
    }
    if parts.is_empty() {
        ["Shared · nobody else typing".into(), "ok".into()]
    } else {
        [
            format!("Shared · {}", parts.join(", ")),
            if typing > 0 { "warn" } else { "info" }.into(),
        ]
    }
}
fn activity_description(pane: &Value, now: f64) -> [String; 2] {
    let how = first(pane, &["activity"]);
    match how.as_str() {
        "working" => return ["Working now".into(), "ok".into()],
        "blocked" => return ["Waiting for you now".into(), "warn".into()],
        _ => (),
    }
    let at = pane.get("activity_at").and_then(number).unwrap_or_default();
    if at == 0.0 {
        return ["Not observed yet".into(), "faint".into()];
    }
    let age = (now - at).max(0.0) as u64;
    let (amount, unit) = if age >= 86400 {
        (age / 86400, "d")
    } else if age >= 3600 {
        (age / 3600, "h")
    } else if age >= 60 {
        (age / 60, "min")
    } else {
        (age, "s")
    };
    let what = match how.as_str() {
        "status" => "Status changed",
        "input" => "Last input",
        "output" => "Last output",
        _ => "Active",
    };
    [format!("{what} {amount} {unit} ago"), "muted".into()]
}
fn title_case(value: &str) -> String {
    let mut word = true;
    let mut result = String::new();
    for c in value.chars() {
        if c.is_alphabetic() {
            if word {
                result.extend(c.to_uppercase());
            } else {
                result.extend(c.to_lowercase());
            }
            word = false;
        } else {
            result.push(c);
            word = true;
        }
    }
    result
}
impl Tree {
    pub fn fields(&self, index: usize, context: &ProjectionContext) -> Value {
        let node = &self.nodes[index];
        let pane = node.pane.as_ref().unwrap_or(&Value::Null);
        if node.kind == "external" {
            let process = node.attr("process");
            let shown = |key| {
                let value = first(&process, &[key]);
                if value.is_empty() {
                    "unavailable".to_owned()
                } else {
                    value
                }
            };
            return json!([
                ["Process", shown("name"), "harness"],
                ["Kind", shown("kind"), "muted"],
                ["PID", shown("pid"), "id"],
                ["Parent", shown("parent_pid"), "muted"],
                ["Terminal", shown("tty"), "muted"],
                ["Folder", shown("cwd"), "path"],
                ["Foreground", shown("foreground"), "muted"],
                ["State", shown("state"), "state"],
                [
                    "Input / preview",
                    "Unavailable · externally owned terminal",
                    "warn"
                ],
                [
                    "Control",
                    "Launch a Daedalus terminal for input and capture",
                    "info"
                ]
            ]);
        }
        if node.kind == "external group" {
            let mut rows = vec![
                json!(["Scope", "Local terminals outside managed backends", "muted"]),
                json!(["Observed", text(&node.attr("external_count")), "info"]),
                json!([
                    "Access",
                    "Process information only · no input or capture",
                    "muted"
                ]),
            ];
            for error in node.attr("errors").as_array().into_iter().flatten() {
                rows.push(json!(["Discovery", text(error), "warn"]));
            }
            return rows.into();
        }
        if node
            .pane
            .as_ref()
            .is_some_and(crate::adapters::pane_observations::conflicted)
        {
            let identity = node
                .pane
                .as_ref()
                .and_then(|pane| crate::adapters::terminals::identity(pane).ok())
                .and_then(|identity| serde_json::to_string(&identity).ok())
                .unwrap_or_else(|| "unverified".into());
            return json!([
                ["Terminal identity", identity, "muted"],
                [
                    "Discovery",
                    crate::adapters::pane_observations::WARNING,
                    "warn"
                ],
                [
                    "Authority",
                    "Conflicting observations; refresh before actions",
                    "warn"
                ]
            ]);
        }
        let checkpoint = node.attr("checkpoint");
        if truth(&checkpoint) {
            let harness = s(&checkpoint, "harness");
            let created = first(&checkpoint, &["created_at"]);
            return json!([
                ["Checkpoint", s(&checkpoint, "name"), "rose"],
                [
                    "Harness",
                    if harness.is_empty() {
                        "not reported".into()
                    } else {
                        title_case(&harness)
                    },
                    if harness.is_empty() {
                        "faint"
                    } else {
                        "harness"
                    }
                ],
                [
                    "Saved",
                    if created.is_empty() {
                        "not recorded"
                    } else {
                        &created
                    },
                    if created.is_empty() { "faint" } else { "muted" }
                ],
                [
                    "Saved profile",
                    if s(&checkpoint, "profile").is_empty() {
                        "not recorded".into()
                    } else {
                        s(&checkpoint, "profile")
                    },
                    if s(&checkpoint, "profile").is_empty() {
                        "faint"
                    } else {
                        "muted"
                    }
                ],
                [
                    "Project",
                    if s(&checkpoint, "cwd").is_empty() {
                        "not recorded".into()
                    } else {
                        s(&checkpoint, "cwd")
                    },
                    "muted"
                ],
                [
                    "Saved source",
                    checkpoint
                        .get("source")
                        .and_then(crate::core::layouts::TerminalRef::decode_identity)
                        .map(|reference| reference.key())
                        .unwrap_or_else(|| "not recorded or unverified".into()),
                    "muted"
                ],
                [
                    "Link",
                    node.attr("checkpoint_match")
                        .get("message")
                        .and_then(Value::as_str)
                        .unwrap_or("Current profile and source affiliation are unverified."),
                    "warn"
                ],
                ["Enter", "View, fork, resume or hand off", "muted"]
            ]);
        }
        match node.kind.as_str() {
            "saved route" => {
                return json!([
                    ["Route", node.label, "accent"],
                    ["Meaning", "Saved route, not a delivery receipt", "muted"]
                ]);
            }
            "nested view" | "declared view" => {
                return json!([
                    ["View", node.label, "info"],
                    ["Source", "Reported by the backend, not inferred", "muted"]
                ]);
            }
            _ => (),
        }
        let backend = node.key.first().map(text).unwrap_or_default();
        if node.pane.is_none() {
            let mut rows = vec![
                json!(["Scope", title_case(&node.kind), "muted"]),
                json!([
                    "Terminals",
                    node.attributes
                        .get("pane_count")
                        .map(text)
                        .unwrap_or("0".into()),
                    "info"
                ]),
            ];
            if node.kind == "server" {
                rows.push(json!([
                    "Server",
                    node.key.get(1).map(text).unwrap_or_default(),
                    backend
                ]));
            }
            return rows.into();
        }
        let sid = native_sid(pane);
        let mut rows = Vec::new();
        let viewer = node.attr("viewer");
        if truth(&viewer) {
            rows.push(json!([
                "View of",
                format!(
                    "{} via {}; closing it closes only the view",
                    viewer
                        .get("source_label")
                        .map(text)
                        .unwrap_or("a terminal".into()),
                    viewer
                        .get("mechanism")
                        .map(text)
                        .unwrap_or("native client".into())
                ),
                "info"
            ]));
        }
        let agent = first(pane, &["agent"]);
        let agent_here = is_agent(pane);
        let state = {
            let p = first(pane, &["agent_status"]);
            if !p.is_empty() {
                p
            } else {
                node.attributes
                    .get("state")
                    .filter(|v| truth(v))
                    .map(text)
                    .unwrap_or("process present".into())
            }
        };
        let [activity, tone] = activity_description(pane, context.now);
        rows.push(json!(["State", state, "state"]));
        rows.push(json!(["Activity", activity, tone]));
        if let Some(usage) = node.attributes.get("usage")
            && let (Some(seconds), Some(visits)) =
                (usage["seconds"].as_f64(), usage["visits"].as_u64())
        {
            rows.push(json!([
                "Herdr selection",
                format!("{:.1} min sampled · {visits} visits", seconds / 60.0),
                "muted"
            ]));
        }
        rows.push(if agent_here {
            json!(["Agent", agent, "harness"])
        } else if !agent.is_empty() {
            json!(["Program", agent, "muted"])
        } else {
            json!(["Agent", "not reported", "faint"])
        });
        let server = first(pane, &["socket", "server", "session"]);
        let native = backend == "native";
        let session = if native {
            first(pane, &["workspace_label", "workspace_id"])
        } else {
            first(pane, &["session"])
        };
        rows.push(json!(["Server", server, backend]));
        let herdr = backend == "herdr";
        if session != server {
            rows.push(json!([
                if native {
                    "Workspace"
                } else if herdr {
                    "Session"
                } else {
                    "tmux session"
                },
                if session.is_empty() {
                    "not reported"
                } else {
                    &session
                },
                if session.is_empty() {
                    "faint"
                } else {
                    &backend
                }
            ]));
        }
        let place = first(pane, &[if herdr { "tab_id" } else { "window" }]);
        let terminal = first(pane, &["terminal_id"]);
        let mut cwd = first(pane, &["foreground_cwd", "cwd"]);
        if !context.home.is_empty()
            && context.home != "/"
            && (cwd == context.home || cwd.starts_with(&format!("{}/", context.home)))
        {
            cwd = format!("~{}", &cwd[context.home.len()..]);
        }
        let split = node
            .attributes
            .get("split")
            .and_then(Value::as_u64)
            .unwrap_or_default();
        let split = if split > 1 {
            format!(" · {split} terminals split here")
        } else {
            String::new()
        };
        rows.extend([
            json!([
                if native {
                    "Terminal · ID"
                } else if herdr {
                    "Tab · pane"
                } else {
                    "Window · pane"
                },
                format!(
                    "{} · {}{split}",
                    if place.is_empty() { "?" } else { &place },
                    s(pane, "pane_id")
                ),
                "muted"
            ]),
            json!([
                "Terminal",
                if terminal.is_empty() {
                    "not reported"
                } else {
                    &terminal
                },
                if terminal.is_empty() { "faint" } else { "id" }
            ]),
            json!([
                "Folder",
                if cwd.is_empty() { "not reported" } else { &cwd },
                if cwd.is_empty() { "faint" } else { "" }
            ]),
        ]);
        let gap = node.attr("sid_gap");
        let why = gap
            .get(1)
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .unwrap_or("not reported");
        let fix = gap.get(2).and_then(Value::as_str).unwrap_or_default();
        if agent_here || !sid.is_empty() {
            rows.push(if sid.is_empty() {
                json!(["Agent session", why, "faint"])
            } else {
                json!(["Agent session", sid, "id"])
            });
        }
        if !fix.is_empty() && sid.is_empty() {
            rows.push(json!(["To fix", fix, "warn"]));
        }
        let info = node.attr_ref("views").unwrap_or(&Value::Null);
        if !herdr {
            let pid = first(pane, &["pid"]);
            rows.push(json!([
                "Process",
                format!(
                    "pid {} · shared by every view",
                    if pid.is_empty() { "?" } else { &pid }
                ),
                "muted"
            ]));
        }
        let places = array(info, "places");
        let count = info["count"].as_u64().unwrap_or(1).max(1);
        let places = places.iter().map(text).collect::<Vec<_>>().join("; ");
        let desc = format!(
            "{count}{}{}",
            if herdr { " known" } else { "" },
            if places.is_empty() {
                String::new()
            } else {
                format!(" · {places}")
            }
        );
        let limited = info["details_limited"] == true;
        let desc = if limited {
            format!("{desc} · details incomplete")
        } else {
            desc
        };
        rows.push(json!(["Views", desc, if limited { "warn" } else { "" }]));
        let [input, tone] = input_brief(info, &backend);
        rows.push(json!(["Input", input, tone]));
        rows.into()
    }
    /// Complete protocol rows; the existing frontend owns collapse and search.
    #[cfg(test)]
    pub fn rows(&self, context: &ProjectionContext) -> Vec<Value> {
        self.filtered_rows(context, &BTreeSet::new(), "")
    }
    pub fn rows_bounded(&self, context: &ProjectionContext, limit: usize) -> (Vec<Value>, bool) {
        self.filtered_rows_bounded(context, &BTreeSet::new(), "", limit)
    }
    #[cfg(test)]
    pub fn filtered_rows(
        &self,
        context: &ProjectionContext,
        collapsed: &BTreeSet<String>,
        query: &str,
    ) -> Vec<Value> {
        self.filtered_rows_bounded(context, collapsed, query, usize::MAX)
            .0
    }
    pub fn outline(&self, limit: usize) -> (String, bool) {
        fn walk(
            tree: &Tree,
            keys: &[usize],
            prefix: &str,
            remaining: &mut usize,
            text: &mut String,
        ) -> bool {
            for (position, index) in keys.iter().copied().enumerate() {
                let last = position + 1 == keys.len();
                let line = format!(
                    "{prefix}{}{}\n",
                    if last { "└─ " } else { "├─ " },
                    tree.nodes[index].label
                );
                let Ok(bytes) = super::discovery_budget::json_size(&line, *remaining) else {
                    return false;
                };
                *remaining -= bytes;
                text.push_str(&line);
                if !walk(
                    tree,
                    &tree.nodes[index].children,
                    &format!("{prefix}{}", if last { "   " } else { "│  " }),
                    remaining,
                    text,
                ) {
                    return false;
                }
            }
            true
        }
        let mut text = String::new();
        let mut remaining = limit;
        let complete = walk(self, &self.roots, "", &mut remaining, &mut text);
        text.pop();
        (text, !complete)
    }
    fn filtered_rows_bounded(
        &self,
        context: &ProjectionContext,
        collapsed: &BTreeSet<String>,
        query: &str,
        limit: usize,
    ) -> (Vec<Value>, bool) {
        let query = lower(query.trim());
        let mut visible = BTreeSet::new();
        if !query.is_empty() {
            for (index, node) in self.nodes.iter().enumerate() {
                let content = format!(
                    "{} {} {} {}",
                    node.label,
                    node.kind,
                    node.pane.as_ref().map(Value::to_string).unwrap_or_default(),
                    node.attributes
                        .get("process")
                        .map(Value::to_string)
                        .unwrap_or_default()
                );
                if lower(&content).contains(&query) {
                    let mut current = Some(index);
                    while let Some(index) = current {
                        if !visible.insert(index) {
                            break;
                        }
                        current = self.nodes[index].parent;
                    }
                }
            }
        }
        struct Filter<'a> {
            query: &'a str,
            visible: &'a BTreeSet<usize>,
            collapsed: &'a BTreeSet<String>,
            pins: &'a std::collections::HashSet<(crate::core::identity::TerminalId, String)>,
        }
        struct RowBudget {
            remaining: usize,
            limited: bool,
        }
        fn walk(
            tree: &Tree,
            keys: &[usize],
            prefix: &str,
            filter: &Filter<'_>,
            context: &ProjectionContext,
            rows: &mut Vec<Value>,
            budget: &mut RowBudget,
        ) {
            let included: Vec<_> = keys
                .iter()
                .copied()
                .filter(|k| filter.query.is_empty() || filter.visible.contains(k))
                .collect();
            for (position, index) in included.iter().copied().enumerate() {
                let last = position + 1 == included.len();
                let node = &tree.nodes[index];
                let id = node.id();
                let attr = |key: &str, default: Value| {
                    node.attributes.get(key).cloned().unwrap_or(default)
                };
                let tile=node.pane.as_ref().filter(|_|node.kind=="pane").map(|pane|{
                    let pin=crate::adapters::terminals::reference(pane).ok().and_then(|reference|reference.identity().ok().map(|id|(id,reference.generation))).is_some_and(|key|filter.pins.contains(&key));
                    let place=[first(pane,&["session"]),first(pane,&["workspace_id","window"]),first(pane,&["pane_id"])].into_iter().filter(|s|!s.is_empty()).collect::<Vec<_>>().join(" / ");
                    json!({"title":title(pane,&context.hostname),"place":place,"project":first(pane,&["session"]),"harness":first(pane,&["agent"]),"state":attr("state","".into()),"views":node.attr_ref("views").and_then(|views|views.get("count")).cloned().unwrap_or(1.into()),"pinned":pin,"viewer":truth(&node.attr("viewer")),"split":attr("split",0.into())})
                });
                let mut row = json!({"id":id,"parent":node.parent.map(|p|tree.nodes[p].id()).unwrap_or_default(),"label":node.label,"parts":attr("parts",json!([])),"branch":format!("{prefix}{}",if last{"└─ "}else{"├─ "}),"kind":node.kind,"backend":node.key.first().map(text).unwrap_or_default(),"children":!node.children.is_empty(),"count":attr("pane_count",0.into()),"state":attr("state","".into()),"active_at":attr("activity_at",json!(0.0)),"activity":attr("activity","".into()),"linked":attr("linked",0.into()),"viewed":attr("viewed",false.into()),"server_focused":attr("server_focused",false.into()),"actionable":node.pane.as_ref().is_some_and(|pane|!crate::adapters::pane_observations::conflicted(pane))||truth(&node.attr("checkpoint")),"fields":tree.fields(index,context),"tile":tile,"layout":node.attr("layout")});
                if let Some(usage) = node.attributes.get("usage") {
                    row["usage"] = usage.clone();
                }
                let separator = usize::from(!rows.is_empty());
                let Ok(bytes) = super::discovery_budget::json_size(
                    &row,
                    budget.remaining.saturating_sub(separator),
                ) else {
                    budget.limited = true;
                    return;
                };
                budget.remaining -= bytes + separator;
                rows.push(row);
                if !filter.query.is_empty() || !filter.collapsed.contains(&id) {
                    walk(
                        tree,
                        &node.children,
                        &format!("{prefix}{}", if last { "   " } else { "│  " }),
                        filter,
                        context,
                        rows,
                        budget,
                    );
                    if budget.limited {
                        return;
                    }
                }
            }
        }
        let pins = context
            .pins
            .iter()
            .filter_map(crate::core::layouts::TerminalRef::decode_identity)
            .filter(|reference| reference.validate().is_ok())
            .filter_map(|reference| {
                reference
                    .identity()
                    .ok()
                    .map(|identity| (identity, reference.generation))
            })
            .collect();
        let mut rows = Vec::new();
        let mut budget = RowBudget {
            remaining: limit,
            limited: false,
        };
        walk(
            self,
            &self.roots,
            "",
            &Filter {
                query: &query,
                visible: &visible,
                collapsed,
                pins: &pins,
            },
            context,
            &mut rows,
            &mut budget,
        );
        (rows, budget.limited)
    }
}
#[cfg(test)]
mod tests {
    #[test]
    fn agy_is_a_terminal_harness_without_inventing_lifecycle_capabilities() {
        assert!(super::is_agent(
            &serde_json::json!({"backend":"tmux","agent":"agy"})
        ));
        assert!(super::is_agent(
            &serde_json::json!({"backend":"herdr","agent":"agy"})
        ));
        assert!(!crate::app::config::HARNESSES.contains(&"agy"));
    }

    use super::*;
    #[test]
    fn linked_view_geometry_stays_with_each_exact_session_and_window() {
        let panes: Vec<_> = (0..2).flat_map(|view| (0..2).map(move |pane| {
            let width = if view == 0 {49} else {79};
            json!({"backend":"tmux","socket":crate::test_support::path("/tmp/views-layout.sock"),"server":"fixture","session":format!("view-{view}"),"session_id":format!("${view}"),"window_id":"@1","pane_id":format!("%{pane}"),"terminal_id":format!("generation-{pane}"),"left":if pane==0 {0}else{width+1},"top":0,"width":if pane==0 {width}else{99-width},"height":40})
        })).collect();
        let tree = Tree::build(
            &json!({"panes":panes}),
            &json!({}),
            &ProjectionContext::default(),
        );
        for node in tree.nodes.iter().filter(|node| node.kind == "pane") {
            let layout = &tree.nodes[node.parent.unwrap()].attributes["layout"];
            let rect = layout["panes"]
                .as_array()
                .expect("each exact view keeps its geometry")
                .iter()
                .find(|r| r["id"] == node.id())
                .unwrap();
            assert_eq!(rect["w"], node.pane.as_ref().unwrap()["width"]);
        }
    }
    #[test]
    fn ambiguous_herdr_geometry_never_picks_one_generation() {
        let (mut fixture, context) = fixture();
        let panes = fixture["snapshot"]["panes"].as_array_mut().unwrap();
        let mut old = panes
            .iter()
            .find(|p| p["backend"] == "herdr" && p["pane_id"] == "w1:p1")
            .unwrap()
            .clone();
        old["terminal_id"] = json!("older-generation");
        panes.push(old);
        let tree = Tree::build(&fixture["snapshot"], &fixture["health"], &context);
        let pane = tree
            .nodes
            .iter()
            .find(|n| {
                n.pane
                    .as_ref()
                    .is_some_and(|p| p["backend"] == "herdr" && p["pane_id"] == "w1:p1")
            })
            .unwrap();
        assert!(
            !tree.nodes[pane.parent.unwrap()]
                .attributes
                .contains_key("layout")
        );
    }
    #[test]
    fn duplicate_layout_rows_keep_tree_children_unique() {
        let (mut fixture, context) = fixture();
        let repeated = fixture["snapshot"]["layouts"][0]["panes"][0].clone();
        fixture["snapshot"]["layouts"][0]["panes"]
            .as_array_mut()
            .unwrap()
            .push(repeated);
        let tree = Tree::build(&fixture["snapshot"], &fixture["health"], &context);
        for node in &tree.nodes {
            assert_eq!(
                node.children.len(),
                node.children.iter().collect::<BTreeSet<_>>().len()
            );
        }
    }
    #[test]
    fn geometry_outside_the_frontend_coordinate_contract_is_omitted() {
        let (mut fixture, context) = fixture();
        fixture["snapshot"]["layouts"][0]["area"]["width"] = json!(70000);
        let tree = Tree::build(&fixture["snapshot"], &fixture["health"], &context);
        assert!(tree.nodes.iter().all(|node| {
            node.attributes
                .get("layout")
                .is_none_or(|layout| layout["width"].as_u64().unwrap() <= u16::MAX as u64)
        }));
    }
    #[test]
    fn optional_relationship_limits_preserve_every_base_terminal_and_exact_view_count() {
        let panes: Vec<_> = (0..40).map(|n|json!({"backend":"tmux","socket":crate::test_support::path("/tmp/outer.sock"),"server":"outer","session":format!("linked-{n}"),"session_id":format!("${n}"),"window_id":"@1","pane_id":"%1","terminal_id":"generation"})).collect();
        let clients: Vec<_> = (0..40).map(|n|json!({"backend":"tmux","socket":crate::test_support::path("/tmp/inner.sock"),"client":format!("inner-{n}"),"session_id":"$inner","pane_id":"%2","flags":"control-mode","outer_panes":[{"socket":crate::test_support::path("/tmp/outer.sock"),"pane_id":"%1"}]})).collect();
        let snapshot = json!({"panes":panes,"clients":clients});
        let health = json!({});
        let mut context = ProjectionContext::default();
        context.runtime.discovery_projection_timeout_ms = 60000;
        context.runtime.discovery_relationship_limit = 12;
        let partial = Tree::build(&snapshot, &health, &context);
        assert!(partial.relationships_limited);
        let ids = |tree: &Tree| {
            tree.nodes
                .iter()
                .filter(|node| node.kind == "pane")
                .map(Node::id)
                .collect::<BTreeSet<_>>()
        };
        let base = ids(&partial);
        assert_eq!(base.len(), 40);
        assert!(
            partial
                .nodes
                .iter()
                .filter(|node| node.kind == "pane")
                .all(|node| node.attr_ref("views").unwrap()["count"] == 40)
        );
        assert!(
            partial
                .nodes
                .iter()
                .filter(|node| node.kind == "nested view")
                .count()
                <= 12
        );
        context.runtime.discovery_relationship_limit = 10000;
        let complete = Tree::build(&snapshot, &health, &context);
        assert!(!complete.relationships_limited);
        assert_eq!(ids(&complete), base);
        assert_eq!(
            complete
                .nodes
                .iter()
                .filter(|node| node.kind == "nested view")
                .count(),
            1600
        );
        context.runtime.discovery_relationship_limit_bytes = 1024;
        let small = Tree::build(&snapshot, &health, &context);
        assert!(small.relationships_limited);
        assert_eq!(ids(&small), base);
        let context = ProjectionContext::default();
        let unknown = input_brief(
            &json!({"clients_complete":false,"typing":0,"watching":0,"control_clients":0}),
            "tmux",
        );
        assert!(unknown[0].contains("unavailable") && !unknown[0].contains("nobody"));
        assert_eq!(context.runtime.discovery_relationship_limit, 16384);
    }
    #[test]
    fn client_summary_storage_grows_with_observations_without_losing_input_counts() {
        let panes: Vec<_> = (0..128).map(|n| json!({"backend":"tmux","socket":crate::test_support::path("/tmp/clients.sock"),"server":"fixture","session":"main","session_id":"$1","window_id":"@1","pane_id":format!("%{n}"),"terminal_id":"generation"})).collect();
        let clients: Vec<_> = (0..128).map(|n| json!({"backend":"tmux","socket":crate::test_support::path("/tmp/clients.sock"),"session_id":"$1","pane_id":"%0","tty":format!("{}-{n}","fixture-client".repeat(8)),"flags":match n%3 {0=>"",1=>"read-only",_=>"control-mode"}})).collect();
        let snapshot = json!({"panes":panes,"clients":clients});
        let context = ProjectionContext::default();
        let tree = Tree::build(&snapshot, &json!({}), &context);
        let retained: usize = tree
            .nodes
            .iter()
            .filter_map(|node| node.attr_ref("views"))
            .map(|info| super::super::discovery_budget::json_size(info, usize::MAX).unwrap())
            .sum();
        let input_bytes = super::super::discovery_budget::json_size(&snapshot, usize::MAX).unwrap();
        eprintln!(
            "Fixture client summaries: {input_bytes} input bytes, {retained} retained view bytes"
        );
        assert!(
            retained <= input_bytes * 3,
            "client labels expanded from {input_bytes} input bytes to {retained} retained view bytes"
        );
        for (index, node) in tree
            .nodes
            .iter()
            .enumerate()
            .filter(|(_, node)| node.kind == "pane")
        {
            let fields = tree.fields(index, &context);
            let input = fields
                .as_array()
                .unwrap()
                .iter()
                .find(|row| row[0] == "Input")
                .unwrap()[1]
                .as_str()
                .unwrap();
            assert_eq!(
                input,
                if node.pane.as_ref().unwrap()["pane_id"] == "%0" {
                    "Shared · 43 typing, 43 watching, 42 control-mode"
                } else {
                    "Shared · 86 watching, 42 control-mode"
                }
            );
        }
    }
    #[test]
    fn linked_badges_and_group_collapse_preserve_the_complete_terminal_identity() {
        let one = json!({"backend":"tmux","socket":crate::test_support::path("/tmp/identity.sock"),"server":"fixture","session":"main","session_id":"$1","window_id":"@1","pane_id":"%1","terminal_id":"same-generation"});
        let mut other = one.clone();
        other["pane_id"] = json!("%2");
        let context = ProjectionContext::default();
        let tree = Tree::build(&json!({"panes":[one.clone(),other]}), &json!({}), &context);
        for node in tree.nodes.iter().filter(|node| node.kind == "pane") {
            assert_eq!(node.attr("linked"), 1);
        }
        let mut old = one.clone();
        old["session_group"] = json!("main");
        let mut replacement = old.clone();
        replacement["terminal_id"] = json!("replacement-generation");
        let tree = Tree::build(&json!({"panes":[old,replacement]}), &json!({}), &context);
        assert_eq!(
            tree.nodes.iter().filter(|node| node.kind == "pane").count(),
            2
        );
        let mut replacement = one.clone();
        replacement["terminal_id"] = json!("replacement-generation");
        let snapshot = json!({"panes":[one,replacement],"daedalus_views":[{"viewer":{"backend":"tmux","endpoint":crate::test_support::path("/tmp/identity.sock"),"pane":"%1","generation":"same-generation"},"source":"herdr/fixture/source/generation","source_label":"source"}]});
        let tree = Tree::build(&snapshot, &json!({}), &context);
        assert_eq!(
            tree.nodes
                .iter()
                .filter(|node| node.kind == "pane" && truth(&node.attr("viewer")))
                .count(),
            1
        );
    }
    #[test]
    fn legacy_display_bindings_cannot_project_routes_onto_unproven_terminals() {
        let (mut fixture, context) = fixture();
        fixture["health"]["native_routes"] = json!([]);
        let mut duplicate = fixture["snapshot"]["panes"]
            .as_array()
            .unwrap()
            .iter()
            .find(|pane| pane["terminal_id"] == "uuid1")
            .unwrap()
            .clone();
        duplicate["pane_id"] = json!("different-pane");
        fixture["snapshot"]["panes"]
            .as_array_mut()
            .unwrap()
            .push(duplicate);
        let tree = Tree::build(&fixture["snapshot"], &fixture["health"], &context);
        assert!(!tree.nodes.iter().any(|node| node.kind == "saved route"));
    }
    #[test]
    fn equal_generations_on_different_panes_are_not_shared_terminal_views() {
        let one = json!({"backend":"herdr","server":"fixture","session":"fixture","pane_id":"one","terminal_id":"generation"});
        let mut other = one.clone();
        other["pane_id"] = json!("two");
        let snapshot = json!({"panes":[one,other]});
        assert_eq!(
            views(
                &snapshot["panes"][0],
                &view_index(&array(&snapshot, "panes").iter().collect::<Vec<_>>()),
                &RelationshipIndex::new(
                    &snapshot,
                    &json!({}),
                    &mut RelationshipBudget::new(&crate::app::runtime_settings::Settings::default()),
                    true,
                ),
                &mut RelationshipBudget::new(&crate::app::runtime_settings::Settings::default())
            )["count"],
            1
        );
    }
    #[test]
    fn actual_linked_views_share_metadata_and_outline_does_not_build_detail_rows() {
        let one = json!({"backend":"tmux","socket":crate::test_support::path("/tmp/views.sock"),"server":"server","session":"one","session_id":"$1","window_id":"@1","pane_id":"%1","terminal_id":"generation"});
        let mut two = one.clone();
        two["session"] = json!("two");
        two["session_id"] = json!("$2");
        let snapshot = json!({"panes":[one,two]});
        let context = ProjectionContext::default();
        let tree = Tree::build(&snapshot, &json!({}), &context);
        let panes = tree
            .nodes
            .iter()
            .filter(|node| node.pane.is_some())
            .collect::<Vec<_>>();
        assert_eq!(panes.len(), 2);
        assert!(std::sync::Arc::ptr_eq(
            panes[0].shared_views.as_ref().unwrap(),
            panes[1].shared_views.as_ref().unwrap()
        ));
        assert_eq!(panes[0].attr_ref("views").unwrap()["count"], 2);
        let expected = tree
            .rows(&context)
            .iter()
            .map(|row| format!("{}{}", s(row, "branch"), s(row, "label")))
            .collect::<Vec<_>>()
            .join("\n");
        assert_eq!(tree.outline(65536), (expected, false));
        let (small, limited) = tree.outline(8);
        assert!(limited && small.len() <= 8);
    }
    fn fixture() -> (Value, ProjectionContext) {
        let value: Value = serde_json::from_str(include_str!(
            "../../../../tests/fixtures/tree-projection.json"
        ))
        .unwrap();
        let c = &value["context"];
        let context = ProjectionContext {
            hostname: s(c, "hostname"),
            home: s(c, "home"),
            now: c["now"].as_f64().unwrap(),
            pins: array(c, "pins").to_vec(),
            ..ProjectionContext::default()
        };
        (value, context)
    }
    #[test]
    fn pin_badges_require_the_supplemental_generation() {
        let pane = json!({"backend":"herdr","server":"fixture","session":"fixture","pane_id":"p1","terminal_id":"terminal","generation":"new-process"});
        let mut context = ProjectionContext {
            pins: vec![
                json!({"backend":"herdr","server":"fixture","pane_id":"p1","terminal_id":"terminal","generation":"old-process"}),
            ],
            ..ProjectionContext::default()
        };
        let tree = Tree::build(&json!({"panes":[pane]}), &json!({}), &context);
        let pinned = |context: &ProjectionContext| {
            tree.rows(context)
                .into_iter()
                .find(|row| row["kind"] == "pane")
                .unwrap()["tile"]["pinned"]
                .clone()
        };
        assert_eq!(pinned(&context), false);
        context.pins[0]["generation"] = json!("new-process");
        assert_eq!(pinned(&context), true);
    }
    #[test]
    fn repeated_identical_pane_observations_count_once_but_distinct_views_survive() {
        let pane = json!({"backend":"herdr","session":"fixture","server":"fixture","workspace_id":"work","tab_id":"one","pane_id":"pane","terminal_id":"generation"});
        let context = ProjectionContext::default();
        let tree = Tree::build(
            &json!({"panes":[pane.clone(),pane.clone()]}),
            &json!({}),
            &context,
        );
        let nodes: Vec<_> = tree
            .nodes
            .iter()
            .filter(|node| node.kind == "pane")
            .collect();
        assert_eq!(nodes.len(), 1);
        assert_eq!(nodes[0].attr("views")["count"], 1);
        assert_eq!(nodes[0].attr("linked"), 1);
        let mut other = pane.clone();
        other["tab_id"] = json!("two");
        let tree = Tree::build(&json!({"panes":[pane,other]}), &json!({}), &context);
        let nodes: Vec<_> = tree
            .nodes
            .iter()
            .filter(|node| node.kind == "pane")
            .collect();
        assert_eq!(nodes.len(), 2);
        assert!(nodes.iter().all(|node| node.attr("views")["count"] == 2));
    }
    #[test]
    fn conflicting_same_view_geometry_is_never_chosen_from_input_order() {
        let first = json!({"backend":"tmux","socket":crate::test_support::path("/tmp/conflicting-layout.sock"),"session":"fixture","session_id":"$1","window_id":"@1","pane_id":"%1","terminal_id":"one","left":0,"top":0,"width":49,"height":24});
        let mut conflict = first.clone();
        conflict["width"] = json!(48);
        let mut second = first.clone();
        second["pane_id"] = json!("%2");
        second["terminal_id"] = json!("two");
        second["left"] = json!(50);
        second["width"] = json!(50);
        for panes in [
            vec![first.clone(), conflict.clone(), second.clone()],
            vec![conflict, first, second],
        ] {
            let tree = Tree::build(
                &json!({"panes":panes}),
                &json!({}),
                &ProjectionContext::default(),
            );
            assert_eq!(
                tree.nodes.iter().filter(|node| node.kind == "pane").count(),
                2
            );
            assert!(
                tree.nodes
                    .iter()
                    .filter(|node| node.kind == "window")
                    .all(|node| node.attr("layout").is_null())
            );
            assert!(!tree.layout_warnings.is_empty());
        }
    }
    #[test]
    fn conflicting_captured_conversations_never_offer_an_arbitrary_actionable_session() {
        let first = json!({"backend":"herdr","session":"fixture","server":"fixture","workspace_id":"work","tab_id":"tab","pane_id":"pane","terminal_id":"generation","agent":"codex","agent_session":{"agent":"codex","kind":"id","value":"session-one"}});
        let mut conflict = first.clone();
        conflict["agent_session"]["value"] = json!("session-two");
        let context = ProjectionContext::default();
        let tree = Tree::build(&json!({"panes":[first,conflict]}), &json!({}), &context);
        let panes: Vec<_> = tree
            .rows(&context)
            .into_iter()
            .filter(|row| row["kind"] == "pane")
            .collect();
        assert_eq!(panes.len(), 1);
        assert_eq!(panes[0]["actionable"], false);
        assert!(!tree.nodes.iter().any(|node| node.kind == "agent session"));
    }
    #[test]
    fn checkpoint_native_id_links_expose_saved_profile_and_affiliation_uncertainty() {
        let pane = json!({"backend":"herdr","session":"fixture","server":"fixture","pane_id":"pane","terminal_id":"generation","agent":"codex","agent_session":{"agent":"codex","kind":"id","value":"same-native-id"}});
        let profiles = [
            crate::test_support::path("/tmp/profile-one"),
            crate::test_support::path("/tmp/profile-two"),
        ];
        let checkpoints:Vec<_>=profiles.iter().enumerate().map(|(index,profile)|json!({"key":format!("checkpoint-{index}"),"name":format!("Saved {index}"),"harness":"codex","sid":"same-native-id","profile":profile,"cwd":crate::test_support::path("/tmp/project")})).collect();
        let context = ProjectionContext::default();
        let tree = Tree::build(
            &json!({"panes":[pane]}),
            &json!({"checkpoints":checkpoints}),
            &context,
        );
        let nodes: Vec<_> = tree
            .nodes
            .iter()
            .enumerate()
            .filter(|(_, node)| node.kind == "checkpoint")
            .collect();
        assert_eq!(nodes.len(), 2);
        for ((index, _), profile) in nodes.into_iter().zip(profiles) {
            let fields = tree.fields(index, &context);
            let text = fields
                .as_array()
                .unwrap()
                .iter()
                .filter_map(|row| row[1].as_str())
                .collect::<Vec<_>>()
                .join(" ");
            assert!(text.contains(profile));
            assert!(text.to_lowercase().contains("unverified"));
        }
    }
    #[test]
    fn checkpoint_tree_policy_filters_by_exact_saved_origin_without_changing_saved_rows() {
        use super::super::interface_settings::CheckpointMatches;
        let pane = json!({"backend":"herdr","session":"fixture","server":"fixture","pane_id":"pane","terminal_id":"generation","generation":"extra","agent":"codex","agent_session":{"agent":"codex","kind":"id","value":"session"}});
        let source =
            serde_json::to_value(crate::adapters::terminals::reference(&pane).unwrap()).unwrap();
        let mut other = source.clone();
        other["generation"] = json!("other-extra");
        let health = json!({"checkpoints":[{"key":"exact","name":"Exact","harness":"codex","sid":"session","source":source},{"key":"candidate","name":"Candidate","harness":"codex","sid":"session","source":other}]});
        let before = health.clone();
        for (mode, count) in [
            (CheckpointMatches::All, 2),
            (CheckpointMatches::Source, 1),
            (CheckpointMatches::Off, 0),
        ] {
            let context = ProjectionContext {
                checkpoint_matches: mode,
                ..ProjectionContext::default()
            };
            let tree = Tree::build(&json!({"panes":[pane]}), &health, &context);
            let nodes: Vec<_> = tree
                .nodes
                .iter()
                .filter(|node| node.kind == "checkpoint")
                .collect();
            assert_eq!(nodes.len(), count);
            if mode == CheckpointMatches::Source {
                assert_eq!(nodes[0].attr("checkpoint")["key"], "exact");
                assert_eq!(nodes[0].attr("checkpoint_match")["kind"], "source_origin");
            }
        }
        assert_eq!(health, before);
    }
    #[test]
    fn ambiguous_legacy_view_source_never_counts_as_a_relationship() {
        let panes: Vec<_> = [("a/b","c"),("a","b/c")].into_iter().map(|(pane,generation)| json!({"backend":"herdr","server":"fixture","session":"fixture","pane_id":pane,"terminal_id":generation})).collect();
        let context = ProjectionContext::default();
        let tree = Tree::build(
            &json!({"panes":panes,"daedalus_views":[{"backend":"herdr","server":"fixture","pane_id":"viewer","terminal_id":"viewer-generation","source":"herdr/fixture/a/b/c"}]}),
            &json!({}),
            &context,
        );
        for node in tree.nodes.iter().filter(|node| node.kind == "pane") {
            assert_eq!(node.attr("views")["count"], 1);
        }
    }
    #[test]
    fn complete_wire_rows_match_python_projection() {
        // Golden generated from control.tree.build/fields and protocol.send_state
        // row projection with the fixture's fixed hostname, home and timestamp.
        // The linked window's missing layout/split count is explicitly corrected
        // from its own reported geometry. The checkpoint's three display fields
        // independently disclose native-ID-only/profile/source uncertainty;
        // all other historical values remain.
        let (fixture, context) = fixture();
        let actual = Tree::build(&fixture["snapshot"], &fixture["health"], &context).rows(&context);
        let expected: Vec<Value> = serde_json::from_str(include_str!(
            "../../../../tests/fixtures/tree-projection-expected.json"
        ))
        .unwrap();
        let changes:Vec<_>=actual.iter().zip(&expected).enumerate().filter_map(|(index,(actual,expected))| {
            if actual==expected {None} else {Some(json!({"index":index,"id":actual["id"],"changes":actual.as_object().unwrap().iter().filter(|(key,value)|expected.get(*key)!=Some(*value)).map(|(key,value)|(key.clone(),json!({"actual":value,"expected":expected[key]}))).collect::<Map<_,_>>() }))}
        }).collect();
        assert!(
            actual.len() == expected.len() && changes.is_empty(),
            "Projection differs: {}",
            json!(changes)
        );
    }
    #[test]
    fn grouped_sessions_views_and_empty_topology_remain_visible() {
        let (fixture, context) = fixture();
        let tree = Tree::build(&fixture["snapshot"], &fixture["health"], &context);
        let rows = tree.rows(&context);
        assert!(rows.iter().any(|r| r["label"] == "main (+1 grouped)"));
        assert!(
            !rows
                .iter()
                .any(|r| r["kind"] == "tmux session" && r["label"] == "main-view")
        );
        assert!(
            rows.iter()
                .any(|r| r["kind"] == "pane" && r["viewed"] == true && r["tile"]["pinned"] == true)
        );
        assert!(
            rows.iter()
                .any(|r| r["label"] == "Empty tab" && r["count"] == 0)
        );
        assert!(rows.iter().any(|r| r["kind"] == "nested view"));
        assert!(rows.iter().any(|r| r["kind"] == "declared view"));
        assert!(
            rows.iter()
                .any(|r| r["kind"] == "checkpoint" && r["actionable"] == true)
        );
        assert!(rows.iter().any(|r| r["kind"] == "saved route"));
    }
    #[test]
    fn stale_selection_cannot_match_reused_pane() {
        let (mut fixture, context) = fixture();
        let tree = Tree::build(&fixture["snapshot"], &fixture["health"], &context);
        let selected = tree
            .nodes
            .iter()
            .find(|n| {
                n.kind == "pane" && n.pane.as_ref().is_some_and(|p| p["terminal_id"] == "uuid1")
            })
            .unwrap()
            .id();
        for pane in fixture["snapshot"]["panes"].as_array_mut().unwrap() {
            if pane["terminal_id"] == "uuid1" {
                pane["terminal_id"] = "replacement".into();
            }
        }
        let refreshed = Tree::build(&fixture["snapshot"], &fixture["health"], &context);
        assert!(refreshed.find(&selected).is_none());
    }
    #[test]
    fn shared_display_label_cannot_attach_one_route_to_two_terminals() {
        let (mut fixture, context) = fixture();
        fixture["health"]["native_routes"] = json!([]);
        fixture["health"]["bindings"].as_array_mut().unwrap().push(
            json!({"backend":"herdr","server":"work","terminal_id":"uuid2","display":"agent1"}),
        );
        let tree = Tree::build(&fixture["snapshot"], &fixture["health"], &context);
        assert!(!tree.nodes.iter().any(|node| node.kind == "saved route"));
    }
    #[test]
    fn native_routes_require_exact_generation_and_ignore_shared_labels() {
        let (mut fixture, context) = fixture();
        fixture["health"]["routes"] = json!({});
        let pane = fixture["snapshot"]["panes"]
            .as_array()
            .unwrap()
            .iter()
            .find(|p| p["terminal_id"] == "uuid1")
            .unwrap()
            .clone();
        let identity = crate::adapters::terminals::identity(&pane).unwrap();
        fixture["health"]["native_routes"] = json!([{
            "scope":{"instance":"fixture","bot":"123","chat":"42"},
            "terminal":identity,"revision":1,"pinned":true
        }]);
        let tree = Tree::build(&fixture["snapshot"], &fixture["health"], &context);
        let routes: Vec<_> = tree
            .nodes
            .iter()
            .filter(|node| node.kind == "saved route")
            .collect();
        assert_eq!(routes.len(), 1);
        assert_eq!(routes[0].label, "Telegram chat · 42");
        // Reuse all pane coordinates and labels with a different generation.
        for live in fixture["snapshot"]["panes"].as_array_mut().unwrap() {
            if live["terminal_id"] == "uuid1" {
                live["terminal_id"] = "replacement".into();
            }
        }
        let stale = Tree::build(&fixture["snapshot"], &fixture["health"], &context);
        assert!(!stale.nodes.iter().any(|node| node.kind == "saved route"));
    }
    #[test]
    fn filtering_retains_ancestors_and_collapse_hides_children() {
        let (fixture, context) = fixture();
        let tree = Tree::build(&fixture["snapshot"], &fixture["health"], &context);
        let collapsed = tree.roots.iter().map(|i| tree.nodes[*i].id()).collect();
        let rows = tree.filtered_rows(&context, &collapsed, "");
        assert_eq!(rows.len(), tree.roots.len());
        let rows = tree.filtered_rows(&context, &collapsed, "Before tests");
        assert_eq!(rows.len(), 6);
        assert_eq!(rows.last().unwrap()["kind"], "checkpoint");
    }
    #[test]
    fn native_session_is_never_inferred_from_title_or_other_harness() {
        let pane = json!({"backend":"herdr","server":"work","session":"work","workspace_id":"w","tab_id":"tab","pane_id":"pane","terminal_id":"uuid","agent":"codex","title":"session-123","agent_session":{"kind":"id","agent":"claude","value":"wrong"}});
        let tree = Tree::build(
            &json!({"panes":[pane]}),
            &json!({}),
            &ProjectionContext::default(),
        );
        let agent = tree
            .nodes
            .iter()
            .find(|n| n.kind == "agent session")
            .unwrap();
        assert_eq!(agent.label, "codex");
        assert_eq!(agent.attr("sid"), "");
    }
    #[test]
    fn windows_and_linux_harness_labels_keep_the_same_reported_conversation() {
        for (agent, reported) in [
            ("codex", "codex"),
            ("CODEX.EXE", "codex"),
            ("C:\\Tools\\codex.exe", "CODEX.EXE"),
            ("cursor-agent.exe", "cursor"),
        ] {
            let pane = json!({"backend":"herdr","server":"fixture","session":"fixture","workspace_id":"w","tab_id":"t","pane_id":"p","terminal_id":"terminal","agent":agent,"agent_session":{"kind":"id","agent":reported,"value":"conversation-id"}});
            assert!(is_agent(&pane));
            assert_eq!(native_sid(&pane), "conversation-id");
            let tree = Tree::build(
                &json!({"panes":[pane]}),
                &json!({}),
                &ProjectionContext::default(),
            );
            assert!(
                tree.nodes
                    .iter()
                    .any(|node| node.kind == "agent session"
                        && node.attr("sid") == "conversation-id")
            );
        }
    }
    #[test]
    fn builtin_terminals_group_by_workspace_and_show_native_details() {
        let context = ProjectionContext::default();
        let pane = json!({"backend":"native","server":"local","session":"a".repeat(64),
            "pane_id":"a".repeat(64),"terminal_id":"b".repeat(64),"session_id":"project",
            "workspace_id":"project","workspace_label":"project","window_id":"a".repeat(64),
            "window":"Developer shell","title":"Developer shell","agent":"bash","cwd":"/project",
            "pid":"42","active":"1","dead":"0"});
        let tree = Tree::build(&json!({"panes":[pane]}), &json!({}), &context);
        let rows = tree.filtered_rows(&context, &BTreeSet::new(), "");
        assert!(rows.iter().any(|row| {
            row["label"]
                .as_str()
                .is_some_and(|label| label.contains("Native terminals"))
        }));
        assert!(rows.iter().any(|row| {
            row["kind"] == "workspace"
                && row["label"]
                    .as_str()
                    .is_some_and(|label| label.contains("project"))
        }));
        let terminal = rows.iter().find(|row| row["actionable"] == true).unwrap();
        let fields = terminal["fields"].to_string();
        assert!(fields.contains("Workspace"));
        assert!(!fields.contains("tmux session"));
        assert!(!fields.contains("Herdr workspace"));
        assert!(fields.contains("one controller at a time"));
        assert!(terminal["tile"].is_object());
    }

    #[test]
    fn native_session_gap_explains_hook_enrollment_without_tmux_dependency() {
        let gap = session_gap(&json!({"backend":"native","agent":"claude"}), &json!({}));
        assert!(gap[1].as_str().unwrap().contains("not yet verified"));
        assert!(gap[2].as_str().unwrap().contains("Open Hooks"));
        assert!(gap[2].as_str().unwrap().contains("explicit session ID"));
        assert!(!gap.to_string().contains("tmux"));
        assert!(!gap.to_string().contains("Herdr"));
    }
    #[test]
    fn external_processes_are_visible_searchable_and_never_actionable() {
        let context = ProjectionContext::default();
        let snapshot = json!({"panes":[], "external_terminals":[{
            "id":"external:42:100", "pid":42, "parent_pid":1, "name":"bash",
            "kind":"terminal", "tty":"/dev/pts/2", "cwd":"/separate/workspace",
            "state":"waiting", "controllable":false
        }]});
        let tree = Tree::build(&snapshot, &json!({}), &context);
        let rows = tree.filtered_rows(&context, &BTreeSet::new(), "");
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0]["label"], "External terminals");
        for row in &rows {
            let fields: Vec<(String, String, String)> =
                serde_json::from_value(row["fields"].clone()).unwrap();
            assert!(!fields.is_empty());
        }
        assert_eq!(rows[0]["fields"][1][1], "1");
        assert_eq!(rows[1]["kind"], "external");
        assert_eq!(rows[1]["actionable"], false);
        assert_eq!(rows[1]["count"], 0);
        assert!(rows[1]["tile"].is_null());
        assert!(tree.nodes.iter().all(|node| node.pane.is_none()));
        assert!(rows[1]["fields"].to_string().contains("Unavailable"));
        assert_eq!(
            tree.filtered_rows(&context, &BTreeSet::new(), "separate/workspace")
                .len(),
            2
        );
    }
    #[test]
    fn external_discovery_failure_is_visible_without_fake_terminal() {
        let context = ProjectionContext::default();
        let tree = Tree::build(
            &json!({"external_errors":["Cannot enumerate local terminal processes"]}),
            &json!({}),
            &context,
        );
        let rows = tree.filtered_rows(&context, &BTreeSet::new(), "");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0]["actionable"], false);
        assert!(rows[0]["fields"].to_string().contains("Cannot enumerate"));
    }
    #[test]
    fn protocol_ids_preserve_python_ascii_encoding() {
        assert_eq!(
            key_id(&["α😀".into(), "quote\"\n".into(), Value::Null]),
            r#"["\u03b1\ud83d\ude00", "quote\"\n", null]"#
        );
    }
    #[test]
    fn graphics_titles_are_hidden_without_hiding_real_titles() {
        for graphics in ["Ga=d,d=I,i=4242,q=2;", "Gi=31,s=1,v=1,a=q,t=d,f=24;AAAA"] {
            assert_eq!(
                title(&json!({"title":graphics,"agent":"herdr"}), "host"),
                "herdr"
            );
        }
        for name in ["Gallery", "G=1", "Ga build"] {
            assert_eq!(title(&json!({"title":name,"agent":"herdr"}), "host"), name);
        }
    }
}
