//! Pure projection of discovered topology into the existing TUI protocol.
//! Display names never identify terminals. Callers supply time/home/hostname;
//! projecting a snapshot performs no filesystem, terminal or network reads.
use serde_json::{Map, Value, json};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone, Default)]
pub struct ProjectionContext {
    pub hostname: String,
    pub home: String,
    pub now: f64,
    pub pins: Vec<Value>,
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
}
impl Node {
    pub fn id(&self) -> String {
        key_id(&self.key)
    }
    fn attr(&self, key: &str) -> Value {
        self.attributes.get(key).cloned().unwrap_or(Value::Null)
    }
}
#[derive(Debug, Clone, Default)]
pub struct Tree {
    pub nodes: Vec<Node>,
    pub roots: Vec<usize>,
    by_id: BTreeMap<String, usize>,
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
    let native = &pane["agent_session"];
    if native["kind"] == "id" && native.get("agent") == pane.get("agent") {
        first(native, &["value"])
    } else {
        String::new()
    }
}
fn session_gap(pane: &Value, health: &Value) -> Value {
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
        });
        self.by_id.insert(id, index);
        if let Some(parent) = parent {
            self.nodes[parent].children.push(index);
        } else {
            self.roots.push(index);
        }
        index
    }
    pub fn find(&self, id: &str) -> Option<&Node> {
        self.by_id.get(id).map(|i| &self.nodes[*i])
    }
    pub fn build(snapshot: &Value, health: &Value, context: &ProjectionContext) -> Self {
        let mut tree = Self::default();
        let mut panes: Vec<&Value> = array(snapshot, "panes").iter().collect();
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
        let mut kept: BTreeMap<(String, String), usize> = BTreeMap::new();
        for (index, pane) in panes.iter().enumerate() {
            if s(pane, "backend") == "tmux" && !first(pane, &["session_group"]).is_empty() {
                groups
                    .entry((s(pane, "socket"), s(pane, "session_group")))
                    .or_default()
                    .insert(s(pane, "session_id"), s(pane, "session"));
                let selected = kept
                    .entry((s(pane, "socket"), s(pane, "pane_id")))
                    .or_insert(index);
                if s(pane, "session") == s(pane, "session_group")
                    && s(panes[*selected], "session") != s(pane, "session")
                {
                    *selected = index;
                }
            }
        }
        panes = panes
            .iter()
            .enumerate()
            .filter(|(i, p)| {
                s(p, "backend") != "tmux"
                    || first(p, &["session_group"]).is_empty()
                    || kept.get(&(s(p, "socket"), s(p, "pane_id"))) == Some(i)
            })
            .map(|(_, p)| *p)
            .collect();
        let identity = |pane: &Value| {
            (
                s(pane, "backend"),
                first(pane, &["socket", "server", "session"]),
                first(pane, &["terminal_id", "pane_id"]),
            )
        };
        let mut identities: BTreeMap<(String, String, String), usize> = BTreeMap::new();
        for pane in &panes {
            *identities.entry(identity(pane)).or_default() += 1;
        }
        let mut placed: BTreeMap<(String, String, String), (usize, usize)> = BTreeMap::new();
        for pane in panes {
            let backend = s(pane, "backend");
            let server = first(pane, &["socket", "server", "session"]);
            let herdr = backend == "herdr";
            let root_key = vec![backend.clone().into(), server.clone().into()];
            let root = tree.add(
                root_key.clone(),
                None,
                format!("{} · {server}", backend.to_uppercase()),
                "server",
                None,
            );
            let project = if herdr {
                v(pane, "workspace_id")
            } else {
                let id = first(pane, &["session_id", "session"]);
                Value::String(id)
            };
            let project_key = extend(&root_key, ["project".into(), project.clone()]);
            let sessions = groups.get(&(s(pane, "socket"), s(pane, "session_group")));
            let project_label = if herdr {
                topology_label(snapshot, "workspaces", &server, &project, &text(&project))
            } else {
                format!(
                    "{}{}",
                    s(pane, "session"),
                    sessions
                        .filter(|g| g.len() > 1)
                        .map(|g| format!(" (+{} grouped)", g.len() - 1))
                        .unwrap_or_default()
                )
            };
            let project_node = tree.add(
                project_key.clone(),
                Some(root),
                project_label,
                if herdr { "workspace" } else { "tmux session" },
                None,
            );
            let group = if herdr {
                v(pane, "tab_id")
            } else {
                let id = first(pane, &["window_id", "window"]);
                Value::String(if id.is_empty() { "?".into() } else { id })
            };
            let group_key = extend(&project_key, ["group".into(), group.clone()]);
            let group_label = if herdr {
                topology_label(snapshot, "tabs", &server, &group, &text(&group))
            } else {
                pane.get("window").map(text).unwrap_or_else(|| text(&group))
            };
            let group_node = tree.add(
                group_key.clone(),
                Some(project_node),
                group_label,
                if herdr { "tab" } else { "window" },
                None,
            );
            let pane_key = extend(
                &group_key,
                [
                    "pane".into(),
                    v(pane, "pane_id"),
                    pane.get("terminal_id").cloned().unwrap_or("".into()),
                ],
            );
            let viewer = array(snapshot, "daedalus_views").iter().find(|viewer| {
                s(viewer, "backend") == backend
                    && s(viewer, "server") == first(pane, &["server", "session"])
                    && viewer.get("pane_id") == pane.get("pane_id")
            });
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
                format!("{} · {title}", s(pane, "pane_id")),
                "pane",
                Some(pane),
            );
            placed.insert(
                (backend.clone(), server.clone(), s(pane, "pane_id")),
                (group_node, pane_node),
            );
            let active_at = pane.get("activity_at").and_then(number).unwrap_or_default();
            let activity = first(pane, &["activity"]);
            let linked = identities.get(&identity(pane)).copied().unwrap_or_default();
            let viewed = backend == "tmux"
                && array(snapshot, "clients").iter().any(|c| {
                    c.get("socket") == pane.get("socket")
                        && c.get("pane_id") == pane.get("pane_id")
                        && sessions
                            .map(|g| g.contains_key(&s(c, "session_id")))
                            .unwrap_or_else(|| c.get("session_id") == pane.get("session_id"))
                });
            let focused = herdr
                && array(snapshot, "herdr_focus").iter().any(|f| {
                    f.get("session") == pane.get("session")
                        && f.get("pane_id") == pane.get("pane_id")
                });
            tree.nodes[pane_node].attributes=json!({"parts":label_parts(pane,&title,&context.hostname),"viewer":viewer,"views":views(pane,snapshot),"state":state,"activity_at":active_at,"activity":activity,"linked":linked,"viewed":viewed,"server_focused":focused}).as_object().unwrap().clone();
            if let Some(key) = super::workspace_usage::key(pane)
                && let Some(usage) = snapshot.get("workspace_usage").and_then(|u| u.get(&key))
            {
                tree.nodes[pane_node]
                    .attributes
                    .insert("usage".into(), usage.clone());
            }
            for client in array(snapshot, "clients") {
                if backend == "tmux"
                    && array(client, "outer_panes").iter().any(|outer| {
                        outer.get("socket") == pane.get("socket")
                            && outer.get("pane_id") == pane.get("pane_id")
                    })
                {
                    let key = extend(
                        &pane_key,
                        [
                            "nested".into(),
                            s(client, "socket").into(),
                            s(client, "client").into(),
                        ],
                    );
                    tree.add(
                        key,
                        Some(pane_node),
                        format!(
                            "Nested tmux view → {} / {}",
                            s(client, "socket"),
                            s(client, "session_id")
                        ),
                        "nested view",
                        None,
                    );
                }
            }
            if backend == "tmux" && !first(pane, &["herdr_session"]).is_empty() {
                tree.add(
                    extend(&pane_key, ["herdr-view".into(), v(pane, "herdr_session")]),
                    Some(pane_node),
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
                    format!(
                        "{harness} · {}",
                        if sid.is_empty() {
                            "session ID not reported"
                        } else {
                            &short
                        }
                    ),
                    "agent session",
                    Some(pane),
                );
                let gap = tree.nodes[pane_node].attr("sid_gap");
                let brief = gap.get(0).and_then(Value::as_str).unwrap_or_default();
                let mut parts = vec![
                    json!([harness, "harness"]),
                    if !sid.is_empty() {
                        json!([short, "sid"])
                    } else {
                        json!([
                            if brief.is_empty() {
                                "session ID not reported"
                            } else {
                                "no session ID"
                            },
                            "muted"
                        ])
                    },
                ];
                if !brief.is_empty() {
                    parts.push(json!([brief, "warn"]));
                }
                tree.nodes[agent_node].attributes=json!({"sid":sid,"sid_gap":gap,"parts":parts,"state":state,"activity_at":active_at,"activity":activity,"linked":linked}).as_object().unwrap().clone();
                for saved in array(health, "checkpoints") {
                    if !sid.is_empty()
                        && s(saved, "sid") == sid
                        && saved.get("harness") == pane.get("agent")
                    {
                        let leaf = tree.add(
                            extend(&key, ["checkpoint".into(), v(saved, "key")]),
                            Some(agent_node),
                            format!("Checkpoint · {}", s(saved, "name")),
                            "checkpoint",
                            None,
                        );
                        tree.nodes[leaf]
                            .attributes
                            .insert("checkpoint".into(), saved.clone());
                    }
                }
            }
            // Native routes carry the complete identity; labels and pane numbers
            // alone must never attach a route to a replacement terminal.
            if let Ok(identity) = crate::adapters::terminals::identity(pane) {
                for saved in array(health, "native_routes") {
                    let Ok(route) =
                        serde_json::from_value::<crate::core::routes::Route>(saved.clone())
                    else {
                        continue;
                    };
                    if route.terminal == identity
                        && route.revision > 0
                        && route.scope.validate().is_ok()
                    {
                        let chat = route.scope.chat;
                        tree.add(
                            extend(&pane_key, ["telegram".into(), chat.clone().into()]),
                            Some(pane_node),
                            format!("Telegram chat · {chat}"),
                            "saved route",
                            None,
                        );
                    }
                }
            }
            let bindings: Vec<_> = array(health, "bindings")
                .iter()
                .filter(|b| {
                    s(b, "backend") == backend
                        && s(b, "server") == first(pane, &["server", "session"])
                        && b.get("terminal_id") == pane.get("terminal_id")
                })
                .collect();
            if bindings.len() == 1
                && bindings[0]
                    .get("display")
                    .and_then(Value::as_str)
                    .is_some_and(|name| !name.is_empty())
                && array(health, "bindings")
                    .iter()
                    .filter(|binding| binding.get("display") == bindings[0].get("display"))
                    .count()
                    == 1
                && let Some(routes) = health.get("routes").and_then(Value::as_object)
            {
                for (chat, target) in routes {
                    if Some(target) == bindings[0].get("display") {
                        tree.add(
                            extend(&pane_key, ["telegram".into(), chat.clone().into()]),
                            Some(pane_node),
                            format!("Telegram chat · {chat}"),
                            "saved route",
                            None,
                        );
                    }
                }
            }
        }
        tree.lay_out(snapshot, &placed);
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
                    topology_label(snapshot, "workspaces", &server, &project, &text(&project)),
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
        tree
    }
}
fn topology_label(
    snapshot: &Value,
    collection: &str,
    server: &str,
    id: &Value,
    fallback: &str,
) -> String {
    array(snapshot, collection)
        .iter()
        .find(|row| {
            s(row, "session") == server
                && row.get(if collection == "workspaces" {
                    "workspace_id"
                } else {
                    "tab_id"
                }) == Some(id)
        })
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
fn views(pane: &Value, snapshot: &Value) -> Value {
    let mut places = Vec::new();
    let mut typing = Vec::new();
    let mut watching = Vec::new();
    let mut control = Vec::new();
    let same: Vec<_> = if truth(&pane["terminal_id"]) {
        array(snapshot, "panes")
            .iter()
            .filter(|p| {
                p.get("backend") == pane.get("backend")
                    && first(p, &["socket", "server"]) == first(pane, &["socket", "server"])
                    && p.get("terminal_id") == pane.get("terminal_id")
            })
            .collect()
    } else {
        vec![pane]
    };
    for p in &same {
        let session = first(p, &["session"]);
        let backend = s(p, "backend");
        let kind = if session.starts_with("daedalus-view-") {
            "Daedalus view session"
        } else if backend == "tmux" {
            "tmux session"
        } else {
            "Herdr workspace"
        };
        let place = if backend == "herdr" {
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
        places.push(format!("{kind} {place}"));
    }
    if s(pane, "backend") == "tmux" {
        for client in array(snapshot, "clients") {
            if s(client, "backend") != "tmux"
                || client.get("socket") != pane.get("socket")
                || !same
                    .iter()
                    .any(|p| p.get("session_id") == client.get("session_id"))
            {
                continue;
            }
            let flags = s(client, "flags");
            let flags: Vec<_> = flags.split(',').collect();
            let label = first(client, &["tty", "client"]);
            let label = format!("client {}", if label.is_empty() { "?" } else { &label });
            if flags.contains(&"control-mode") {
                control.push(label);
            } else if client.get("pane_id") != pane.get("pane_id") {
                watching.push(format!("{label} (another pane is active)"));
            } else if flags.contains(&"read-only") {
                watching.push(format!("{label} (read-only)"));
            } else {
                typing.push(label);
            }
        }
    }
    let source = ["backend", "server", "pane_id", "terminal_id"]
        .map(|k| first(pane, &[k]))
        .join("/");
    for viewer in array(snapshot, "daedalus_views") {
        if s(viewer, "source") == source {
            places.push(format!("Daedalus viewer pane {}", s(viewer, "label")));
        }
    }
    json!({"count":places.len(),"places":places,"typing":typing,"watching":watching,"control_clients":control,"shared":s(pane,"backend")=="tmux"})
}
fn input_brief(info: &Value, backend: &str) -> [String; 2] {
    if backend == "herdr" {
        return [
            "One attach slot · asks before takeover".into(),
            "muted".into(),
        ];
    }
    let typing = array(info, "typing").len();
    let mut parts = Vec::new();
    for (count, name) in [
        (typing, "typing"),
        (array(info, "watching").len(), "watching"),
        (array(info, "control_clients").len(), "control-mode"),
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
        let session = first(pane, &["session"]);
        rows.push(json!(["Server", server, backend]));
        let herdr = backend == "herdr";
        if session != server {
            rows.push(json!([
                if herdr { "Session" } else { "tmux session" },
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
                if herdr {
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
        let info = node.attr("views");
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
        let places = array(&info, "places");
        let count = places.len().max(1);
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
        rows.push(json!(["Views", desc, ""]));
        let [input, tone] = input_brief(&info, &backend);
        rows.push(json!(["Input", input, tone]));
        rows.into()
    }
    /// Complete protocol rows; the existing frontend owns collapse and search.
    pub fn rows(&self, context: &ProjectionContext) -> Vec<Value> {
        self.filtered_rows(context, &BTreeSet::new(), "")
    }
    pub fn filtered_rows(
        &self,
        context: &ProjectionContext,
        collapsed: &BTreeSet<String>,
        query: &str,
    ) -> Vec<Value> {
        let query = lower(query.trim());
        let mut visible = BTreeSet::new();
        if !query.is_empty() {
            for (index, node) in self.nodes.iter().enumerate() {
                let content = format!(
                    "{} {} {}",
                    node.label,
                    node.kind,
                    node.pane.as_ref().map(Value::to_string).unwrap_or_default()
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
        }
        fn walk(
            tree: &Tree,
            keys: &[usize],
            prefix: &str,
            filter: &Filter<'_>,
            context: &ProjectionContext,
            rows: &mut Vec<Value>,
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
                    let pin=context.pins.iter().any(|r|r.get("backend")==pane.get("backend")&&s(r,"server")==first(pane,&["server","session"])&&r.get("pane_id")==pane.get("pane_id")&&s(r,"terminal_id")==first(pane,&["terminal_id"]));
                    let place=[first(pane,&["session"]),first(pane,&["workspace_id","window"]),first(pane,&["pane_id"])].into_iter().filter(|s|!s.is_empty()).collect::<Vec<_>>().join(" / ");
                    json!({"title":title(pane,&context.hostname),"place":place,"project":first(pane,&["session"]),"harness":first(pane,&["agent"]),"state":attr("state","".into()),"views":node.attr("views").get("count").cloned().unwrap_or(1.into()),"pinned":pin,"viewer":truth(&node.attr("viewer")),"split":attr("split",0.into())})
                });
                rows.push(json!({"id":id,"parent":node.parent.map(|p|tree.nodes[p].id()).unwrap_or_default(),"label":node.label,"parts":attr("parts",json!([])),"branch":format!("{prefix}{}",if last{"└─ "}else{"├─ "}),"kind":node.kind,"backend":node.key.first().map(text).unwrap_or_default(),"children":!node.children.is_empty(),"count":attr("pane_count",0.into()),"state":attr("state","".into()),"active_at":attr("activity_at",json!(0.0)),"activity":attr("activity","".into()),"linked":attr("linked",0.into()),"viewed":attr("viewed",false.into()),"server_focused":attr("server_focused",false.into()),"actionable":node.pane.is_some()||truth(&node.attr("checkpoint")),"fields":tree.fields(index,context),"tile":tile,"layout":node.attr("layout")}));
                if let (Some(usage), Some(row)) = (node.attributes.get("usage"), rows.last_mut()) {
                    row["usage"] = usage.clone();
                }
                if !filter.query.is_empty() || !filter.collapsed.contains(&id) {
                    walk(
                        tree,
                        &node.children,
                        &format!("{prefix}{}", if last { "   " } else { "│  " }),
                        filter,
                        context,
                        rows,
                    );
                }
            }
        }
        let mut rows = Vec::new();
        walk(
            self,
            &self.roots,
            "",
            &Filter {
                query: &query,
                visible: &visible,
                collapsed,
            },
            context,
            &mut rows,
        );
        rows
    }
}
#[derive(Default)]
struct GroupLayout {
    size: Option<[u64; 2]>,
    zoomed: bool,
    panes: Vec<(usize, [u64; 4], bool)>,
}
fn cells<const N: usize>(value: &Value, keys: [&str; N]) -> Option<[u64; N]> {
    let mut result = [0; N];
    for (i, key) in keys.iter().enumerate() {
        let number = value.get(*key)?;
        let n = number
            .as_u64()
            .or_else(|| number.as_str().and_then(|s| s.parse().ok()))?;
        if n > 1_000_000 {
            return None;
        }
        result[i] = n;
    }
    Some(result)
}
impl Tree {
    fn lay_out(
        &mut self,
        snapshot: &Value,
        placed: &BTreeMap<(String, String, String), (usize, usize)>,
    ) {
        let mut groups: BTreeMap<usize, GroupLayout> = BTreeMap::new();
        for layout in array(snapshot, "layouts") {
            let server = first(layout, &["session"]);
            let Some(size) = cells(&layout["area"], ["width", "height"]) else {
                continue;
            };
            for item in array(layout, "panes") {
                let Some(rect) = cells(&item["rect"], ["x", "y", "width", "height"]) else {
                    continue;
                };
                let Some((group, pane)) =
                    placed.get(&("herdr".into(), server.clone(), s(item, "pane_id")))
                else {
                    continue;
                };
                groups
                    .entry(*group)
                    .or_insert_with(|| GroupLayout {
                        size: Some(size),
                        zoomed: truth(&layout["zoomed"]),
                        panes: Vec::new(),
                    })
                    .panes
                    .push((*pane, rect, truth(&item["focused"])));
            }
        }
        for pane in array(snapshot, "panes") {
            if s(pane, "backend") != "tmux" {
                continue;
            }
            let Some(rect) = cells(pane, ["left", "top", "width", "height"]) else {
                continue;
            };
            let Some((group, node)) =
                placed.get(&("tmux".into(), s(pane, "socket"), s(pane, "pane_id")))
            else {
                continue;
            };
            let group = groups.entry(*group).or_default();
            if !group.panes.iter().any(|(p, _, _)| p == node) {
                group.panes.push((*node, rect, pane["active"] == "1"));
            }
        }
        for (index, mut group) in groups {
            let width = group
                .panes
                .iter()
                .map(|(_, r, _)| r[0] + r[2])
                .max()
                .unwrap_or_default()
                .max(group.size.map(|s| s[0]).unwrap_or_default());
            let height = group
                .panes
                .iter()
                .map(|(_, r, _)| r[1] + r[3])
                .max()
                .unwrap_or_default()
                .max(group.size.map(|s| s[1]).unwrap_or_default());
            group.panes.sort_by_key(|(_, rect, _)| (rect[1], rect[0]));
            let rects:Vec<_>=group.panes.iter().map(|(index,r,focused)|json!({"id":self.nodes[*index].id(),"x":r[0],"y":r[1],"w":r[2],"h":r[3],"focused":focused})).collect();
            self.nodes[index].attributes.insert(
                "layout".into(),
                json!({"width":width,"height":height,"zoomed":group.zoomed,"panes":rects}),
            );
            let order: Vec<_> = group.panes.iter().map(|(index, _, _)| *index).collect();
            let children = &self.nodes[index].children;
            let mut sorted: Vec<_> = order
                .iter()
                .copied()
                .filter(|i| children.contains(i))
                .collect();
            sorted.extend(children.iter().copied().filter(|i| !order.contains(i)));
            self.nodes[index].children = sorted;
            for index in &order {
                self.nodes[*index]
                    .attributes
                    .insert("split".into(), order.len().into());
            }
        }
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
        };
        (value, context)
    }
    #[test]
    fn complete_wire_rows_match_python_projection() {
        // Golden generated from control.tree.build/fields and protocol.send_state
        // row projection with the fixture's fixed hostname, home and timestamp.
        let (fixture, context) = fixture();
        let actual = Tree::build(&fixture["snapshot"], &fixture["health"], &context).rows(&context);
        let expected: Vec<Value> = serde_json::from_str(include_str!(
            "../../../../tests/fixtures/tree-projection-expected.json"
        ))
        .unwrap();
        assert_eq!(actual, expected);
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
        assert!(agent.label.ends_with("session ID not reported"));
        assert_eq!(agent.attr("sid"), "");
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
