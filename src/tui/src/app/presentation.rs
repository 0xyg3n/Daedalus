//! Pure projections for the existing TUI. Observations enter through Context;
//! this module never discovers processes, reads files, or starts services.
use super::{
    config::ControlConfig,
    registry::{self, Badge, Item, SectionSnapshot},
};
use crate::core::layouts::{self, Layouts, Resolution, Step, TerminalRef};
use serde::Serialize;
use serde_json::Value;
use std::{collections::BTreeMap, path::Path};

pub struct Context<'a> {
    pub config: &'a ControlConfig,
    pub config_dir: &'a Path,
    pub home: Option<&'a Path>,
    pub snapshot: &'a Value,
    pub health: &'a Value,
    pub local: &'a Value,
    pub layouts: Option<&'a Layouts>,
    pub show_saved: bool,
    pub demo: bool,
    pub refresh_error: bool,
    pub pending: bool,
    pub last_success: Option<f64>,
    pub now: f64,
    /// File-existence observation only; never proof of valid authentication.
    pub telegram_token_saved: Option<bool>,
}
impl Context<'_> {
    fn fresh(&self) -> bool {
        !self.refresh_error
            && self
                .last_success
                .is_some_and(|at| at.is_finite() && at >= 0.0)
    }
}
#[derive(Clone, Debug, Serialize)]
pub struct Connection {
    pub label: String,
    pub state: String,
    pub detail: String,
}
#[derive(Clone, Debug, Serialize)]
pub struct SetupStep {
    pub label: String,
    pub done: bool,
    pub detail: String,
}
#[derive(Clone, Debug, Serialize)]
pub struct Overview {
    pub connections: Vec<Connection>,
    pub setup: Vec<SetupStep>,
}
#[derive(Clone, Debug, Serialize)]
pub struct Presentation {
    pub sections: Vec<SectionSnapshot>,
    pub overview: Overview,
    pub freshness: String,
}

fn text<'a>(value: &'a Value, key: &str) -> &'a str {
    value.get(key).and_then(Value::as_str).unwrap_or("")
}
fn rows<'a>(value: &'a Value, key: &str) -> &'a [Value] {
    value
        .get(key)
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[])
}
fn truth(value: &Value, key: &str) -> bool {
    value.get(key).and_then(Value::as_bool) == Some(true)
}
fn running_service(service: &Value) -> bool {
    text(service, "state") == "Running"
        && service
            .get("pid")
            .and_then(Value::as_u64)
            .is_some_and(|pid| pid > 0)
        && !text(service, "start").is_empty()
}
fn badge(label: &str, tone: &str) -> Badge {
    (label.into(), tone.into())
}
fn badges(value: &Value, key: &str) -> Vec<Badge> {
    rows(value, key)
        .iter()
        .filter_map(|pair| Some((pair.get(0)?.as_str()?.into(), pair.get(1)?.as_str()?.into())))
        .collect()
}
fn title(value: &str) -> String {
    let mut upper = true;
    value
        .chars()
        .flat_map(|c| {
            let result = if upper {
                c.to_uppercase().collect::<String>()
            } else {
                c.to_lowercase().collect()
            };
            upper = !c.is_alphanumeric();
            result.chars().collect::<Vec<_>>()
        })
        .collect()
}
fn short(path: &str, home: Option<&Path>) -> String {
    if let Some(home) = home
        && let Ok(rest) = Path::new(path).strip_prefix(home)
    {
        return if rest.as_os_str().is_empty() {
            "~".into()
        } else {
            format!("~/{}", rest.display())
        };
    }
    path.into()
}
fn item(
    id: String,
    group: &str,
    name: &str,
    note: String,
    icon: &str,
    labels: Vec<Badge>,
    aliases: Vec<String>,
) -> Item {
    let status = if id.starts_with("hooks:")
        || id.starts_with("switch:")
        || id == "goal:"
        || id.starts_with("armonia:")
    {
        labels.first().cloned().unwrap_or_default()
    } else {
        Badge::default()
    };
    Item {
        id,
        group: group.into(),
        title: name.into(),
        note,
        icon: icon.into(),
        context: String::new(),
        status,
        badges: labels,
        aliases,
    }
}
fn unavailable(id: &str, group: &str, name: &str) -> Item {
    item(
        id.into(),
        group,
        name,
        "Observation unavailable; refresh or open diagnostics.".into(),
        "warning",
        vec![badge("Unavailable", "warn")],
        vec![],
    )
}

pub fn project(context: &Context<'_>) -> Presentation {
    let mut records = BTreeMap::new();
    records.insert(registry::SESSIONS, checkpoint_rows(context.health));
    records.insert(registry::WORKSPACES, layout_rows(context));
    records.insert(registry::INSTANCES, instance_rows(context));
    records.insert(
        registry::HOOKS,
        hook_rows(context.health.get("hooks").unwrap_or(&Value::Null), context),
    );
    records.insert(
        registry::ARMONIA,
        armonia_rows(
            context.health.get("armonia").unwrap_or(&Value::Null),
            context.fresh(),
        ),
    );
    Presentation {
        sections: registry::builtin().section_snapshots(&records, context.show_saved),
        overview: overview(context),
        freshness: freshness(context),
    }
}

pub fn freshness(context: &Context<'_>) -> String {
    if context.demo {
        return "DEMO · synthetic data".into();
    }
    if context.refresh_error {
        return "STALE · refresh failed · r Retry".into();
    }
    if !context.fresh() {
        return "CHECKING · discovering local state".into();
    }
    let age = (context.now - context.last_success.unwrap_or(context.now)).max(0.0) as u64;
    let state = if context.pending {
        "UPDATING"
    } else if !rows(context.snapshot, "errors").is_empty()
        || !rows(context.health, "errors").is_empty()
    {
        "PARTIAL"
    } else {
        "CHECKED"
    };
    format!("{state} · {age}s ago")
}

pub fn overview(context: &Context<'_>) -> Overview {
    let observed = context.local.get("instances").and_then(Value::as_array);
    let mine = observed.and_then(|instances| {
        instances
            .iter()
            .find(|instance| Path::new(text(instance, "directory")) == context.config_dir)
    });
    let services = mine
        .and_then(|instance| instance.get("services"))
        .and_then(Value::as_array);
    let connection = if !context.fresh() || services.is_none() {
        Connection {
            label: "Telegram".into(),
            state: "unknown".into(),
            detail: "Unknown".into(),
        }
    } else {
        let count = services
            .into_iter()
            .flatten()
            .filter(|service| {
                text(service, "kind") == "Telegram feeder" && running_service(service)
            })
            .count();
        Connection {
            label: "Telegram".into(),
            state: if count > 0 { "running" } else { "absent" }.into(),
            detail: if count > 0 {
                format!("{count} running")
            } else {
                "Not detected".into()
            },
        }
    };
    let tools = context
        .config
        .subscriptions
        .iter()
        .map(|name| title(name))
        .collect::<Vec<_>>();
    let count = if context.fresh() {
        live_refs(context.snapshot).len()
    } else {
        0
    };
    let saved = context
        .health
        .get("checkpoints")
        .and_then(Value::as_array)
        .filter(|_| context.fresh())
        .map(Vec::len);
    let telegram = &context.config.telegram;
    let authoritative = context.health.get("telegram");
    let saved_details = authoritative
        .map(|state| state["configured"] == true)
        .unwrap_or_else(|| telegram.bot_id.is_some());
    let authorized = authoritative
        .map(|state| {
            state["configuration"]["grants"]
                .as_array()
                .is_some_and(|grants| !grants.is_empty())
        })
        .unwrap_or_else(|| telegram.owner_id.is_some());
    let token = if context.telegram_token_saved == Some(true) {
        " · token saved"
    } else {
        ""
    };
    Overview {
        connections: vec![connection],
        setup: vec![
            SetupStep {
                label: "AI tools".into(),
                done: !tools.is_empty(),
                detail: if tools.is_empty() {
                    "None selected yet".into()
                } else {
                    tools.join(", ")
                },
            },
            SetupStep {
                label: "Terminals".into(),
                done: count > 0,
                detail: if !context.fresh() {
                    "Observation unavailable".into()
                } else if count > 0 {
                    format!("{count} found")
                } else {
                    "None found yet".into()
                },
            },
            SetupStep {
                label: "Telegram".into(),
                done: authorized && saved_details,
                detail: if authoritative.is_some_and(|state| state["status"] == "unknown") {
                    "Configuration unreadable".into()
                } else if saved_details {
                    format!("Details saved{token}")
                } else {
                    "Optional · not set up".into()
                },
            },
            SetupStep {
                label: "Checkpoints".into(),
                done: saved.is_some_and(|count| count > 0),
                detail: match saved {
                    Some(0) => "Optional · none saved".into(),
                    Some(count) => format!("{count} saved"),
                    None => "Observation unavailable".into(),
                },
            },
        ],
    }
}

fn checkpoint_rows(health: &Value) -> Vec<Item> {
    rows(health, "checkpoints")
        .iter()
        .filter_map(|row| {
            let key = text(row, "key");
            if key.is_empty() {
                return None;
            }
            let native = !text(row, "sid").is_empty();
            Some(item(
                format!("checkpoint:{key}"),
                "Saved checkpoints",
                text(row, "name"),
                format!(
                    "{} · {} · {}",
                    title(text(row, "harness")),
                    if native {
                        "Native ID saved"
                    } else {
                        "Context only"
                    },
                    text(row, "created_at")
                ),
                "checkpoint",
                vec![if native {
                    badge("Native session ID", "ok")
                } else {
                    badge("Context only", "info")
                }],
                vec![text(row, "harness").into()],
            ))
        })
        .collect()
}

fn live_refs(snapshot: &Value) -> Vec<TerminalRef> {
    let mut result = Vec::new();
    for pane in rows(snapshot, "panes") {
        if let Ok(identity) = crate::adapters::terminals::identity(pane) {
            let reference = TerminalRef {
                backend: identity.backend,
                server: identity.endpoint,
                pane_id: identity.pane,
                terminal_id: identity.generation,
                generation: text(pane, "generation").into(),
                label: String::new(),
                place: String::new(),
            };
            if !result
                .iter()
                .any(|existing: &TerminalRef| existing.same_identity(&reference))
            {
                result.push(reference);
            }
        }
    }
    result
}
fn layout_rows(context: &Context<'_>) -> Vec<Item> {
    let Some(layouts) = context.layouts else {
        return vec![];
    };
    let live = if context.fresh() {
        live_refs(context.snapshot)
    } else {
        vec![]
    };
    let mut result = Vec::new();
    for (name, layout) in &layouts.layouts {
        let missing = layout
            .pins
            .iter()
            .filter(|reference| !matches!(layouts::resolve(reference, &live), Resolution::Live(_)))
            .count();
        let mode = serde_json::to_value(layout.mode)
            .ok()
            .and_then(|value| value.as_str().map(title))
            .unwrap_or_else(|| "Tiles".into());
        let saved = layout
            .preferences
            .get("saved_at")
            .and_then(Value::as_str)
            .unwrap_or("")
            .chars()
            .take(10)
            .collect::<String>();
        let missing_note = if missing > 0 {
            format!(" · {missing} missing")
        } else {
            String::new()
        };
        let labels = if !context.fresh() {
            vec![badge("Unverified", "warn")]
        } else if missing > 0 {
            vec![badge(&format!("{missing} missing"), "warn")]
        } else {
            vec![]
        };
        result.push(item(
            format!("layout:{name}"),
            "Saved layouts",
            name,
            format!(
                "{mode} · {} pinned{missing_note} · saved {saved}",
                layout.pins.len()
            ),
            "layout",
            labels,
            vec!["layout".into(), "preset".into()],
        ));
    }
    for (name, workflow) in &layouts.workflows {
        let changes = workflow
            .steps
            .iter()
            .any(|step| matches!(step, Step::RouteTelegram { .. }));
        result.push(item(
            format!("workflow:{name}"),
            "Quick workflows",
            name,
            format!(
                "{} steps{}",
                workflow.steps.len(),
                if changes {
                    " · asks before: route a telegram chat to a terminal"
                } else {
                    ""
                }
            ),
            "workflow",
            if changes {
                vec![badge("Asks before changes", "warn")]
            } else {
                vec![]
            },
            vec!["workflow".into(), "automation".into(), "recipe".into()],
        ));
    }
    result
}

fn instance_rows(context: &Context<'_>) -> Vec<Item> {
    let mut found: Vec<_> = rows(context.local, "instances")
        .iter()
        .filter(|instance| {
            rows(instance, "services").iter().any(running_service)
                || context.show_saved && !rows(instance, "files").is_empty()
        })
        .collect();
    found.sort_by_key(|instance| {
        (
            !rows(instance, "services").iter().any(running_service),
            text(instance, "name").to_lowercase(),
        )
    });
    found
        .into_iter()
        .map(|instance| {
            let count = rows(instance, "services")
                .iter()
                .filter(|service| running_service(service))
                .count();
            let running = count > 0 && context.fresh();
            let directory = text(instance, "directory");
            let labels = if !context.fresh() {
                vec![badge("Unverified", "warn")]
            } else if Path::new(directory) == context.config_dir {
                vec![badge("Active", "ok")]
            } else {
                vec![]
            };
            item(
                format!("instance:{directory}"),
                if running {
                    "Running instances"
                } else {
                    "Saved configurations"
                },
                text(instance, "name"),
                if !context.fresh() || instance.get("services").is_none_or(Value::is_null) {
                    "Process observation unavailable".into()
                } else if running {
                    format!("{count} processes running")
                } else {
                    "Saved configuration · no running process detected".into()
                },
                if running {
                    "instance"
                } else {
                    "instance-saved"
                },
                labels,
                vec![directory.into()],
            )
        })
        .collect()
}

pub fn hook_rows(view: &Value, context: &Context<'_>) -> Vec<Item> {
    if !view.is_object() {
        return vec![unavailable("hooks:status", "Your AI tools", "Hook status")];
    }
    let mut result = Vec::new();
    for tool in rows(view, "tools") {
        let id = text(tool, "tool");
        if id.is_empty() {
            continue;
        }
        let issues = badges(tool, "issues")
            .into_iter()
            .filter(|(_, tone)| matches!(tone.as_str(), "danger" | "warn"))
            .collect::<Vec<_>>();
        let files = rows(tool, "files")
            .iter()
            .filter(|file| truth(file, "exists"))
            .collect::<Vec<_>>();
        let mut note = if let Some(file) = files.first() {
            short(text(file, "path"), context.home)
        } else {
            format!(
                "No hook file · {}",
                short(text(tool, "target"), context.home)
            )
        };
        if !files.is_empty() {
            let events = rows(tool, "events").len();
            note.push_str(&if events > 0 {
                format!(" · {events} events")
            } else {
                " · no hooks".into()
            });
        }
        let mut labels = badges(tool, "labels");
        if labels.is_empty() {
            labels.push(badge("Not configured", ""));
        }
        let mut status = labels[0].clone();
        let herdr = tool.get("herdr").unwrap_or(&Value::Null);
        let state = text(herdr, "state");
        if !state.is_empty() {
            let (label, tone) = match state {
                "current" => ("Herdr hook current".into(), "ok"),
                "outdated" => ("Herdr hook outdated".into(), "warn"),
                "absent" => ("Herdr hook not installed".into(), ""),
                _ => (format!("Herdr hook {state}"), ""),
            };
            labels.push(badge(&label, tone));
            if state == "outdated" {
                status = badge("Herdr outdated", "warn");
            }
        }
        if !issues.is_empty() {
            let label = format!(
                "{} problem{}",
                issues.len(),
                if issues.len() == 1 { "" } else { "s" }
            );
            status = badge(
                &label,
                if issues.iter().any(|(_, tone)| tone == "danger") {
                    "danger"
                } else {
                    "warn"
                },
            );
            labels.push(status.clone());
        }
        let mut row = item(
            format!("hooks:{id}"),
            "Your AI tools",
            text(tool, "name"),
            note,
            id,
            labels,
            vec![
                id.into(),
                "hook file".into(),
                "wiring".into(),
                "herdr".into(),
            ],
        );
        row.status = status;
        result.push(row);
    }
    let group = format!("Switches · {}", text(view, "directory"));
    for switch in rows(view, "switches") {
        let key = text(switch, "key");
        if key.is_empty() {
            continue;
        }
        let on = switch.get("on").and_then(Value::as_bool);
        let mut labels = vec![match on {
            Some(true) => badge("On", "ok"),
            Some(false) => badge(
                "Off",
                if text(switch, "kind") == "guard" {
                    "danger"
                } else {
                    ""
                },
            ),
            None => badge("Unknown", "warn"),
        }];
        if text(switch, "kind") == "telegram" {
            labels.push(if truth(switch, "ready") {
                badge("Sends to Telegram", "warn")
            } else {
                badge("Needs Telegram", "warn")
            });
        }
        result.push(item(
            format!("switch:{key}"),
            &group,
            text(switch, "title"),
            text(switch, "effect").into(),
            "toggle",
            labels,
            vec![key.into(), "flag".into(), "toggle".into()],
        ));
    }
    if view.get("switches").is_some() || view.get("goals").is_some() {
        let observed_goals = view.get("goals").and_then(Value::as_array);
        let goals = observed_goals.map(Vec::as_slice).unwrap_or(&[]);
        let mut counts = BTreeMap::<String, usize>::new();
        for goal in goals {
            let state = goal
                .get("status")
                .and_then(|status| {
                    status
                        .as_str()
                        .or_else(|| status.get("state").and_then(Value::as_str))
                })
                .or_else(|| goal.get("state").and_then(Value::as_str))
                .unwrap_or("unknown");
            *counts.entry(state.into()).or_default() += 1;
        }
        let active = [
            "running",
            "pending",
            "blocked",
            "armed",
            "paused",
            "completed",
        ]
        .into_iter()
        .find(|state| counts.contains_key(*state));
        let status = if !context.fresh() || observed_goals.is_none() {
            badge("Unknown", "warn")
        } else {
            match active {
                Some("running") => badge("Running", "ok"),
                Some("pending") => badge("Pending", "warn"),
                Some("blocked") => badge("Blocked", "warn"),
                Some("armed") => badge("Armed", "warn"),
                Some("paused") => badge("Paused", ""),
                Some("completed") => badge("Completed", "ok"),
                Some(_) => badge("Unknown", "warn"),
                None if goals.is_empty() => badge("Off", ""),
                None => badge("Unknown", "warn"),
            }
        };
        let note = if observed_goals.is_none() {
            "Goal observation unavailable; refresh or inspect diagnostics.".into()
        } else if goals.is_empty() {
            "Keeps an agent working until its goal holds; set with /goal in its chat.".into()
        } else {
            format!(
                "{} · {}",
                counts
                    .iter()
                    .map(|(state, count)| format!("{count} {state}"))
                    .collect::<Vec<_>>()
                    .join(", "),
                text(&goals[0], "directive")
            )
        };
        result.push(item(
            "goal:".into(),
            &group,
            "Goal loop",
            note,
            "workflow",
            vec![status],
            vec!["goal".into(), "/goal".into(), "autonomy".into()],
        ));
    }
    result
}

fn ago(age: Option<f64>) -> String {
    let Some(age) = age else {
        return "never".into();
    };
    let age = age.max(0.0) as u64;
    if age < 90 {
        format!("{age} s ago")
    } else if age < 5400 {
        format!("{} min ago", age / 60)
    } else if age < 172800 {
        format!("{} h ago", age / 3600)
    } else {
        format!("{} days ago", age / 86400)
    }
}

pub fn armonia_rows(view: &Value, fresh: bool) -> Vec<Item> {
    if !view.is_object() {
        return vec![unavailable("armonia:service", "Organizer", "Organizer")];
    }
    let mut result = Vec::new();
    let services = view.get("services").and_then(Value::as_array);
    let mine = services.and_then(|services| services.iter().find(|service| truth(service, "mine")));
    let (note, labels) = if !fresh || services.is_none() {
        (
            "Observation unavailable".into(),
            vec![badge("Unknown", "warn")],
        )
    } else if let Some(service) = mine {
        (
            format!(
                "Running · Herdr session {} · every {} s",
                text(service, "session"),
                service
                    .get("interval")
                    .map(Value::to_string)
                    .unwrap_or_else(|| "?".into())
            ),
            vec![badge("Running", "ok")],
        )
    } else if services.is_some_and(|services| !services.is_empty()) {
        (
            format!(
                "{} organizer(s) running for other configurations",
                services.map_or(0, Vec::len)
            ),
            vec![badge("Not for this configuration", "warn")],
        )
    } else {
        (
            "Not running · start it from a shell; see Workspaces docs".into(),
            vec![badge("Stopped", "")],
        )
    };
    result.push(item(
        "armonia:service".into(),
        "Organizer",
        "Organizer",
        note,
        "status",
        labels,
        vec!["armonia".into(), "organizer".into()],
    ));
    let policy = view.get("policy").unwrap_or(&Value::Null);
    let (note, labels) = if !policy.is_object() {
        (
            "Observation unavailable".into(),
            vec![badge("Unknown", "warn")],
        )
    } else if !text(policy, "error").is_empty() {
        (
            "Policy cannot be read; open diagnostics".into(),
            vec![badge("Unreadable", "warn")],
        )
    } else if !truth(policy, "exists") {
        (
            "No policy file · the organizer holds until one exists".into(),
            vec![badge("None", "")],
        )
    } else {
        let counts = policy
            .get("counts")
            .and_then(Value::as_object)
            .map(|counts| {
                counts
                    .iter()
                    .map(|(key, count)| format!("{count} {key}"))
                    .collect::<Vec<_>>()
                    .join(", ")
            })
            .unwrap_or_default();
        let labels =
            if mine.is_some_and(|service| text(service, "session") != text(policy, "server")) {
                vec![badge("Session mismatch", "danger")]
            } else {
                vec![]
            };
        (
            format!(
                "Herdr session {} · {}",
                if text(policy, "server").is_empty() {
                    "?"
                } else {
                    text(policy, "server")
                },
                if counts.is_empty() {
                    "no entries yet"
                } else {
                    &counts
                }
            ),
            labels,
        )
    };
    result.push(item(
        "armonia:policy".into(),
        "Organizer",
        "Organization policy",
        note,
        "sources",
        labels,
        vec!["armonia".into(), "organizer".into()],
    ));
    let cycle = view.get("cycle").unwrap_or(&Value::Null);
    let (note, labels) = if !cycle.is_object() {
        (
            "Observation unavailable".into(),
            vec![badge("Unknown", "warn")],
        )
    } else if !text(cycle, "error").is_empty() {
        (
            "State cannot be read; open diagnostics".into(),
            vec![badge("Unreadable", "warn")],
        )
    } else if cycle.get("last_poll").is_none_or(Value::is_null) {
        ("No completed cycle recorded".into(), vec![])
    } else {
        let mut labels = Vec::new();
        if view
            .get("journal")
            .and_then(|journal| journal.get("unresolved"))
            .and_then(Value::as_u64)
            .unwrap_or(0)
            > 0
            || view
                .get("pending_renames")
                .and_then(Value::as_u64)
                .unwrap_or(0)
                > 0
        {
            labels.push(badge("Held: outcome unknown", "danger"));
        }
        if truth(cycle, "ordering_hold") {
            labels.push(badge("Tab order kept", "info"));
        }
        let age = cycle
            .get("age")
            .and_then(Value::as_f64)
            .filter(|age| age.is_finite() && *age >= 0.0);
        let interval = mine
            .and_then(|service| service.get("interval"))
            .and_then(Value::as_f64)
            .filter(|interval| interval.is_finite() && *interval > 0.0)
            .unwrap_or(0.0);
        if !fresh || age.is_some_and(|age| age > (3.0 * interval).max(120.0)) {
            let index = usize::from(labels.first().is_some_and(|label| label.1 == "danger"));
            labels.insert(index, badge("Stale", "warn"));
        } else if age.is_none() {
            let index = usize::from(labels.first().is_some_and(|label| label.1 == "danger"));
            labels.insert(index, badge("Unknown", "warn"));
        }
        if labels.is_empty() {
            labels.push(if mine.is_some() {
                badge("Current", "ok")
            } else {
                badge("Recent", "info")
            });
        }
        let agents = cycle
            .get("agents")
            .filter(|value| !value.is_null())
            .map(|count| format!(" · {count} agents"))
            .unwrap_or_default();
        (
            format!(
                "{} {}{agents}",
                if text(cycle, "state").is_empty() {
                    "Unknown".into()
                } else {
                    title(text(cycle, "state"))
                },
                ago(age)
            ),
            labels,
        )
    };
    result.push(item(
        "armonia:cycle".into(),
        "Organizer",
        "Last cycle",
        note,
        "history",
        labels,
        vec!["armonia".into(), "organizer".into()],
    ));
    if !text(view, "error").is_empty() {
        result.push(item(
            "armonia:registry".into(),
            "Organizer",
            "Registry",
            "Cannot be read; open diagnostics".into(),
            "warning",
            vec![badge("Unreadable", "warn")],
            vec!["armonia.sqlite3".into()],
        ));
    }
    let agents = rows(view, "agents");
    let mut remembered = 0;
    for agent in agents {
        if text(agent, "availability") != "live" || !fresh {
            remembered += 1;
            continue;
        }
        let id = agent
            .get("id")
            .map(|id| {
                id.as_str()
                    .map(str::to_owned)
                    .unwrap_or_else(|| id.to_string())
            })
            .unwrap_or_default();
        if id.is_empty() {
            continue;
        }
        let mut parts = vec![
            if text(agent, "workspace").is_empty() {
                "no workspace"
            } else {
                text(agent, "workspace")
            }
            .to_owned(),
            if text(agent, "category").is_empty() {
                "?"
            } else {
                text(agent, "category")
            }
            .to_owned(),
        ];
        if !matches!(text(agent, "harness"), "" | "unknown") {
            parts.push(text(agent, "harness").into());
        }
        if let Some(entry) = policy
            .get("entries")
            .and_then(|entries| entries.get(&id))
            .and_then(Value::as_str)
        {
            parts.push(entry.into());
        }
        let mut labels = vec![badge("Live", "ok")];
        if let Some(rank) = agent.get("pin_rank").filter(|rank| !rank.is_null()) {
            labels.push(badge(&format!("Pinned {rank}"), "info"));
        }
        if text(agent, "category_origin") == "manual" {
            labels.push(badge("Your category", "accent"));
        }
        result.push(item(
            format!("armonia:agent:{id}"),
            "Live agents",
            text(agent, "display"),
            parts.join(" · "),
            "agent",
            labels,
            vec![
                text(agent, "harness").into(),
                text(agent, "workspace").into(),
            ],
        ));
    }
    if remembered > 0 {
        result.push(item(
            "armonia:remembered".into(),
            "Remembered agents",
            &if fresh {
                format!("{remembered} not running now")
            } else {
                format!("{remembered} not verified now")
            },
            "Kept with their names, aliases and history; Enter lists them.".into(),
            "missing",
            if fresh {
                vec![]
            } else {
                vec![badge("Unverified", "warn")]
            },
            vec!["absent".into(), "unavailable".into(), "history".into()],
        ));
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    fn context<'a>(
        config: &'a ControlConfig,
        snapshot: &'a Value,
        health: &'a Value,
        local: &'a Value,
    ) -> Context<'a> {
        Context {
            config,
            config_dir: Path::new("/tmp/current"),
            home: Some(Path::new("/tmp/home")),
            snapshot,
            health,
            local,
            layouts: None,
            show_saved: false,
            demo: false,
            refresh_error: false,
            pending: false,
            last_success: Some(10.0),
            now: 12.0,
            telegram_token_saved: None,
        }
    }
    #[test]
    fn absent_observations_never_claim_running_or_connected() {
        let config = ControlConfig::fresh(Path::new("/tmp/work")).unwrap();
        let empty = json!({});
        let context = context(&config, &empty, &empty, &empty);
        let view = project(&context);
        assert_eq!(view.overview.connections.len(), 1);
        assert_eq!(view.overview.connections[0].state, "unknown");
        assert!(!view.overview.setup[1].done);
        assert_eq!(
            view.sections[registry::ARMONIA].items[0].status.0,
            "Unavailable"
        );
    }
    #[test]
    fn telegram_processes_are_scoped_to_the_selected_instance() {
        let config = ControlConfig::fresh(Path::new("/tmp/work")).unwrap();
        let empty = json!({});
        let local = json!({"instances":[{"directory":"/tmp/other","services":[{"kind":"Telegram feeder","state":"Running","pid":123,"start":"42"}]},{"directory":"/tmp/current","services":[]}]});
        let context = context(&config, &empty, &empty, &local);
        assert_eq!(overview(&context).connections[0].state, "absent");
        let local = json!({"instances":[{"directory":"/tmp/current","services":[{"kind":"Telegram feeder","state":"Running","pid":123,"start":"42"}]}]});
        let mut context = Context {
            local: &local,
            ..context
        };
        assert_eq!(overview(&context).connections[0].state, "running");
        context.refresh_error = true;
        assert_eq!(overview(&context).connections[0].state, "unknown");
    }
    #[test]
    fn saved_credentials_are_settings_not_connection_proof() {
        let mut config = ControlConfig::fresh(Path::new("/tmp/work")).unwrap();
        config.telegram.owner_id = Some("1".into());
        config.telegram.bot_id = Some("2".into());
        let empty = json!({});
        let mut context = context(&config, &empty, &empty, &empty);
        context.telegram_token_saved = Some(true);
        let overview = overview(&context);
        assert!(overview.setup[2].done);
        assert_eq!(overview.setup[2].detail, "Details saved · token saved");
        assert_eq!(overview.connections[0].state, "unknown");
    }
    #[test]
    fn goal_status_uses_observed_state_without_calling_armed_running() {
        let config = ControlConfig::fresh(Path::new("/tmp/work")).unwrap();
        let empty = json!({});
        let mut context = context(&config, &empty, &empty, &empty);
        for (state, label) in [
            ("armed", "Armed"),
            ("running", "Running"),
            ("pending", "Pending"),
            ("blocked", "Blocked"),
            ("paused", "Paused"),
            ("completed", "Completed"),
        ] {
            let view =
                json!({"switches":[],"goals":[{"directive":"Test","status":{"state":state}}]});
            let rows = hook_rows(&view, &context);
            assert_eq!(rows.last().unwrap().id, "goal:");
            assert_eq!(rows.last().unwrap().status.0, label);
        }
        context.refresh_error = true;
        assert_eq!(
            hook_rows(&json!({"goals":[{"status":"running"}]}), &context)
                .last()
                .unwrap()
                .status
                .0,
            "Unknown"
        );
    }
    #[test]
    fn checkpoint_controls_precede_saved_records_and_freshness_is_explicit() {
        let config = ControlConfig::fresh(Path::new("/tmp/work")).unwrap();
        let empty = json!({});
        let health = json!({"checkpoints":[{"key":"dashboard-checkpoints.json:0","name":"Saved","harness":"claude","sid":"session-a","created_at":"today"}]});
        let mut context = context(&config, &empty, &health, &empty);
        let view = project(&context);
        assert_eq!(
            view.sections[registry::SESSIONS].items[0].id,
            "checkpoint.save"
        );
        assert_eq!(
            view.sections[registry::SESSIONS].items[1].id,
            "checkpoint:dashboard-checkpoints.json:0"
        );
        assert_eq!(view.freshness, "CHECKED · 2s ago");
        context.refresh_error = true;
        assert_eq!(freshness(&context), "STALE · refresh failed · r Retry");
    }
    #[test]
    fn saved_instances_hidden_until_requested_and_stale_agents_not_live() {
        let config = ControlConfig::fresh(Path::new("/tmp/work")).unwrap();
        let empty = json!({});
        let local = json!({"instances":[{"directory":"/tmp/saved","name":"Saved","services":[],"files":["control.json"]}]});
        let mut context = context(&config, &empty, &empty, &local);
        assert!(instance_rows(&context).is_empty());
        context.show_saved = true;
        assert_eq!(instance_rows(&context)[0].id, "instance:/tmp/saved");
        let armonia = json!({"agents":[{"id":7,"availability":"live","display":"Test"}]});
        let rows = armonia_rows(&armonia, false);
        assert!(!rows.iter().any(|row| row.id == "armonia:agent:7"));
        assert_eq!(rows.last().unwrap().status.0, "Unverified");
    }

    #[test]
    fn stopped_and_unverified_processes_are_not_running_instances() {
        let config = ControlConfig::fresh(Path::new("/tmp/work")).unwrap();
        let empty = json!({});
        let local = json!({"instances":[{"directory":"/tmp/current","name":"Current","files":["control.json"],"services":[{"kind":"Telegram feeder","state":"Stopped","pid":123,"start":"42"},{"kind":"Telegram feeder","state":"Running","pid":124}]}]});
        let mut context = context(&config, &empty, &empty, &local);
        assert!(instance_rows(&context).is_empty());
        context.show_saved = true;
        let saved = instance_rows(&context);
        assert_eq!(saved[0].group, "Saved configurations");
        assert_eq!(
            saved[0].note,
            "Saved configuration · no running process detected"
        );
        let local = json!({"instances":[{"directory":"/tmp/current","name":"Current","services":[{"state":"Stopped","pid":123,"start":"42"},{"state":"Running","pid":124,"start":"43"},{"state":"Running","pid":125}]}]});
        context.local = &local;
        assert_eq!(instance_rows(&context)[0].note, "1 processes running");
    }

    #[test]
    fn failed_checkpoint_observation_is_unknown_and_health_errors_are_partial() {
        let config = ControlConfig::fresh(Path::new("/tmp/work")).unwrap();
        let empty = json!({});
        let health = json!({"errors":["Saved checkpoints could not be read"]});
        let mut context = context(&config, &empty, &health, &empty);
        assert_eq!(
            overview(&context).setup[3].detail,
            "Observation unavailable"
        );
        assert!(!overview(&context).setup[3].done);
        assert!(freshness(&context).starts_with("PARTIAL"));
        let malformed = json!({"checkpoints":null});
        context.health = &malformed;
        assert_eq!(
            overview(&context).setup[3].detail,
            "Observation unavailable"
        );
        let observed = json!({"checkpoints":[]});
        context.health = &observed;
        assert_eq!(overview(&context).setup[3].detail, "Optional · none saved");
        context.refresh_error = true;
        assert_eq!(
            overview(&context).setup[3].detail,
            "Observation unavailable"
        );
    }

    #[test]
    fn absent_goal_observations_never_claim_the_loop_is_off_or_armed() {
        let config = ControlConfig::fresh(Path::new("/tmp/work")).unwrap();
        let empty = json!({});
        let context = context(&config, &empty, &empty, &empty);
        for view in [
            json!({"switches":[]}),
            json!({"goals":null}),
            json!({"goals":[{"directive":"Unverified flag"}]}),
        ] {
            assert_eq!(
                hook_rows(&view, &context).last().unwrap().status.0,
                "Unknown"
            );
        }
        assert_eq!(
            hook_rows(&json!({"goals":[]}), &context)
                .last()
                .unwrap()
                .status
                .0,
            "Off"
        );
    }

    #[test]
    fn organizer_cycle_requires_fresh_age_and_current_service_evidence() {
        let state = |view: Value| {
            armonia_rows(&view, true)
                .into_iter()
                .find(|row| row.id == "armonia:cycle")
                .unwrap()
                .status
                .0
        };
        assert_eq!(
            state(
                json!({"services":[],"cycle":{"last_poll":1,"age":1_000_000,"state":"completed"}})
            ),
            "Stale"
        );
        assert_eq!(
            state(json!({"services":[],"cycle":{"last_poll":1,"state":"completed"}})),
            "Unknown"
        );
        assert_eq!(
            state(json!({"services":[],"cycle":{"last_poll":1,"age":1,"state":"completed"}})),
            "Recent"
        );
        assert_eq!(
            state(
                json!({"services":[{"mine":true,"interval":60}],"cycle":{"last_poll":1,"age":150,"state":"completed"}})
            ),
            "Current"
        );
        assert_eq!(
            state(
                json!({"services":[{"mine":true,"interval":60}],"cycle":{"last_poll":1,"age":181,"state":"completed"}})
            ),
            "Stale"
        );
    }
}
