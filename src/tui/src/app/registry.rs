//! Canonical navigation and command metadata for the existing TUI wire format.
//! Guided actions preserve the original navigation contract; command references
//! describe the maintained native interfaces.
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::OnceLock,
};

pub const OVERVIEW: usize = 0;
pub const WORKSPACES: usize = 1;
pub const SESSIONS: usize = 2;
pub const CONNECTIONS: usize = 3;
pub const TOOLS: usize = 4;
pub const HOOKS: usize = 5;
pub const ACTIVITY: usize = 6;
pub const INSTANCES: usize = 7;
pub const ARMONIA: usize = 8;
pub const HELP: usize = 9;
pub const COMMANDS: usize = 10;
pub const PAGES: usize = 11;
pub type Badge = (String, String);

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Section {
    pub title: String,
    pub icon: String,
    pub category: String,
    pub summary: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Action {
    pub id: String,
    pub key: String,
    pub page: usize,
    pub group: String,
    pub title: String,
    pub note: String,
    pub icon: String,
    pub effects: Vec<String>,
    pub aliases: Vec<String>,
    pub context: String,
    pub context_note: String,
    pub listed: bool,
    pub quick: String,
}
/// Only GuidedAction entries may be routed to a local action handler. The other
/// variants open reference text, never a shell or an impersonated Telegram sender.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Execution {
    GuidedAction,
    TelegramReference,
    ShellReference,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CatalogEntry {
    pub id: String,
    pub category: String,
    pub group: String,
    pub title: String,
    pub description: String,
    pub interface: String,
    pub detail: String,
    pub aliases: Vec<String>,
    pub icon: String,
    pub badges: Vec<Badge>,
    pub execution: Execution,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Registry {
    pub schema_version: u32,
    pub sections: Vec<Section>,
    pub actions: Vec<Action>,
    pub effects: BTreeMap<String, Badge>,
    pub terminal_menu: Vec<String>,
    pub quick_labels: BTreeMap<String, String>,
    pub references: Vec<CatalogEntry>,
}
#[derive(Clone, Debug, Serialize)]
pub struct Item {
    pub id: String,
    pub group: String,
    pub title: String,
    pub note: String,
    pub icon: String,
    pub context: String,
    pub badges: Vec<Badge>,
    pub aliases: Vec<String>,
    pub status: Badge,
}
#[derive(Clone, Debug, Serialize)]
pub struct SectionSnapshot {
    #[serde(flatten)]
    pub section: Section,
    pub items: Vec<Item>,
}
#[derive(Clone, Debug, Serialize)]
pub struct TerminalAction {
    pub id: String,
    pub short: String,
    pub title: String,
    pub note: String,
    pub quick: String,
    pub icon: String,
    pub badges: Vec<Badge>,
}

pub fn builtin() -> &'static Registry {
    static REGISTRY: OnceLock<Registry> = OnceLock::new();
    REGISTRY.get_or_init(|| {
        let registry: Registry = serde_json::from_str(include_str!("../../assets/commands.json"))
            .expect("Checked command registry JSON must be valid");
        registry
            .validate()
            .expect("Checked command registry contract must be valid");
        registry
    })
}

impl Registry {
    pub fn action(&self, id: &str) -> Option<&Action> {
        self.actions.iter().find(|action| action.id == id)
    }
    pub fn by_key(&self, key: &str) -> Option<&Action> {
        self.actions.iter().find(|action| action.key == key)
    }
    pub fn page_actions(&self, page: usize) -> impl Iterator<Item = &Action> {
        self.actions
            .iter()
            .filter(move |action| action.page == page && action.listed)
    }
    pub fn badges(&self, action: &Action) -> Vec<Badge> {
        action
            .effects
            .iter()
            .filter_map(|effect| self.effects.get(effect).cloned())
            .collect()
    }
    pub fn home(&self, action: &Action) -> String {
        let section = &self.sections[action.page].title;
        if action.listed {
            format!("{section} › {}", action.group)
        } else {
            format!("{section} › select a terminal › Enter")
        }
    }
    /// Dynamic records are supplied separately, then grouped in the same order
    /// as the previous presentation layer (controls before saved records).
    pub fn section_snapshots(
        &self,
        records: &BTreeMap<usize, Vec<Item>>,
        show_saved: bool,
    ) -> Vec<SectionSnapshot> {
        self.sections
            .iter()
            .enumerate()
            .map(|(page, section)| {
                let mut items = records.get(&page).cloned().unwrap_or_default();
                items.extend(self.page_actions(page).map(|action| Item {
                    id: action.id.clone(),
                    group: action.group.clone(),
                    title: if action.id == "instances.saved" && show_saved {
                        "Hide saved configurations".into()
                    } else {
                        action.title.clone()
                    },
                    note: action.note.clone(),
                    icon: action.icon.clone(),
                    context: action.context.clone(),
                    badges: self.badges(action),
                    aliases: action.aliases.clone(),
                    status: (String::new(), String::new()),
                }));
                let mut groups = Vec::new();
                for item in &items {
                    if !groups.contains(&item.group) {
                        groups.push(item.group.clone());
                    }
                }
                if matches!(page, SESSIONS | INSTANCES | WORKSPACES) {
                    groups.sort_by_key(|group| {
                        matches!(
                            group.as_str(),
                            "Saved checkpoints" | "Saved layouts" | "Quick workflows"
                        )
                    });
                } else if page == ARMONIA {
                    groups.sort_by_key(|group| {
                        matches!(group.as_str(), "Live agents" | "Remembered agents")
                    });
                }
                items.sort_by_key(|item| {
                    groups
                        .iter()
                        .position(|group| group == &item.group)
                        .unwrap_or(groups.len())
                });
                SectionSnapshot {
                    section: section.clone(),
                    items,
                }
            })
            .collect()
    }
    pub fn terminal_actions(&self) -> Vec<TerminalAction> {
        self.terminal_menu
            .iter()
            .map(|id| {
                let action = self.action(id).expect("Validated terminal action");
                TerminalAction {
                    id: id.clone(),
                    short: self.quick_labels.get(id).cloned().unwrap_or_default(),
                    title: if action.context.is_empty() {
                        action.title.clone()
                    } else {
                        action.context.clone()
                    },
                    note: if action.context_note.is_empty() {
                        action.note.clone()
                    } else {
                        action.context_note.clone()
                    },
                    quick: action.quick.clone(),
                    icon: action.icon.clone(),
                    badges: self.badges(action),
                }
            })
            .collect()
    }
    pub fn catalog(&self) -> Vec<CatalogEntry> {
        let mut entries: Vec<_> = self.actions.iter().map(|action| {
            let context = if !action.context.is_empty() && action.listed { format!("\nAlso in a terminal's Enter menu as “{}”.", action.context) } else { String::new() };
            CatalogEntry {
                id: format!("action:{}", action.id), category: "Control panel".into(), group: self.sections[action.page].title.clone(), title: action.title.clone(), description: action.note.clone(), interface: "Guided control".into(),
                detail: format!("Home: {}{context}\nEnter runs it here; terminal actions first ask for an exact terminal.", self.home(action)), aliases: action.aliases.clone(), icon: action.icon.clone(), badges: self.badges(action), execution: Execution::GuidedAction,
            }
        }).collect();
        entries.extend(self.references.iter().cloned());
        const CATEGORIES: [&str; 4] = [
            "Control panel",
            "Telegram",
            "Command line",
            "Hooks & maintenance",
        ];
        entries.sort_by_key(|entry| {
            CATEGORIES
                .iter()
                .position(|category| *category == entry.category)
                .unwrap_or(CATEGORIES.len())
        });
        entries
    }
    /// Caller still enforces runtime capability and authorization. This resolves
    /// metadata only; a reference or compatibility alias never becomes a command.
    pub fn catalog_action(&self, entry: &str) -> Option<&Action> {
        entry.strip_prefix("action:").and_then(|id| self.action(id))
    }

    pub fn validate(&self) -> Result<(), &'static str> {
        if self.schema_version != 1 || self.sections.len() != PAGES {
            return Err("Invalid navigation schema");
        }
        let mut ids = BTreeSet::new();
        let mut keys = BTreeSet::new();
        for action in &self.actions {
            if !ids.insert(&action.id)
                || !keys.insert(&action.key)
                || action.page >= COMMANDS
                || action.key.starts_with("page:")
                || action.id.starts_with("voice.")
            {
                return Err("Invalid or duplicate action");
            }
            if action
                .effects
                .iter()
                .any(|effect| !self.effects.contains_key(effect))
            {
                return Err("Unknown action effect");
            }
            if !action.listed && !self.terminal_menu.contains(&action.id) {
                return Err("Unlisted action has no terminal menu home");
            }
        }
        let mut terminal_ids = BTreeSet::new();
        let mut quick = BTreeSet::new();
        for id in &self.terminal_menu {
            let action = self.action(id).ok_or("Unknown terminal action")?;
            if !terminal_ids.insert(id)
                || action.context.is_empty()
                || !action.effects.iter().any(|effect| effect == "target")
            {
                return Err("Invalid terminal menu entry");
            }
            if !action.quick.is_empty()
                && (action.quick.chars().count() != 1 || !quick.insert(&action.quick))
            {
                return Err("Duplicate or invalid terminal shortcut");
            }
        }
        let mut entries = BTreeSet::new();
        let mut titles = BTreeSet::new();
        let mut spellings = BTreeSet::new();
        for entry in self.catalog() {
            if !entries.insert(entry.id.clone())
                || !titles.insert((entry.category.clone(), entry.title.clone()))
            {
                return Err("Duplicate catalog entry");
            }
            if entry.execution != Execution::GuidedAction && entry.id.starts_with("action:") {
                return Err("Reference masquerades as executable action");
            }
            if entry.execution == Execution::GuidedAction
                && self.catalog_action(&entry.id).is_none()
            {
                return Err("Unknown guided catalog action");
            }
            if entry.category == "Telegram" {
                for spelling in std::iter::once(&entry.title).chain(entry.aliases.iter()) {
                    if !spellings.insert(spelling.clone()) {
                        return Err("Duplicate Telegram command or alias");
                    }
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sha2::{Digest, Sha256};
    fn fingerprint(value: &impl Serialize) -> String {
        let value = serde_json::to_value(value).unwrap();
        format!("{:x}", Sha256::digest(serde_json::to_vec(&value).unwrap()))
    }
    #[test]
    fn metadata_matches_reviewed_action_and_navigation_contract() {
        let registry = builtin();
        assert_eq!(registry.actions.len(), 42);
        assert_eq!(registry.sections.len(), 11);
        // Reviewed catalog: release checks replace the redundant install action.
        // Native-client guidance now includes the built-in backend.
        // Navigation retains the established action contract.
        assert_eq!(
            fingerprint(&registry.actions),
            "b7bf3086362b0358a1c9059edf2c39d681bcac45330bfc604e317f00246229e9"
        );
        assert_eq!(
            fingerprint(&registry.sections),
            "6704f3242d8b39a5293a7b8846955a44d275b8bbabd7c002f751b9f07d6dc00c"
        );
    }
    #[test]
    fn each_action_keeps_one_home_and_terminal_shortcuts_keep_order() {
        let registry = builtin();
        registry.validate().unwrap();
        let sections = registry.section_snapshots(&BTreeMap::new(), false);
        let ids: Vec<_> = sections
            .iter()
            .flat_map(|section| section.items.iter().map(|item| &item.id))
            .collect();
        for action in &registry.actions {
            assert_eq!(
                ids.iter().filter(|id| ***id == action.id).count(),
                usize::from(action.listed)
            );
        }
        let terminal = registry.terminal_actions();
        assert_eq!(
            terminal
                .iter()
                .map(|action| action.id.as_str())
                .collect::<Vec<_>>(),
            [
                "terminal.attach",
                "terminal.watch",
                "terminal.view",
                "terminal.focus",
                "terminal.native",
                "checkpoint.save",
                "terminal.fork",
                "terminal.duplicate",
                "terminal.views",
                "terminal.pin",
                "terminal.send",
                "telegram.route",
                "terminal.rename",
                "terminal.organize",
                "terminal.interrupt",
                "terminal.close"
            ]
        );
        assert_eq!(
            terminal
                .iter()
                .filter(|action| !action.quick.is_empty())
                .map(|action| action.quick.as_str())
                .collect::<Vec<_>>(),
            ["a", "w", "c", "f", "d", "p", "t"]
        );
    }
    #[test]
    fn aliases_are_search_metadata_and_native_references_never_execute() {
        let registry = builtin();
        let catalog = registry.catalog();
        assert_eq!(
            catalog
                .iter()
                .filter(|entry| entry.execution == Execution::GuidedAction)
                .count(),
            42
        );
        for entry in &registry.references {
            assert!(registry.catalog_action(&entry.id).is_none());
            assert_ne!(entry.execution, Execution::GuidedAction);
        }
        for (alias, owner) in [
            ("/jobs", "/windows"),
            ("/verbose", "/reason"),
            ("/ctrlc", "/interrupt"),
        ] {
            assert_eq!(
                catalog
                    .iter()
                    .find(|entry| entry.aliases.iter().any(|a| a == alias))
                    .unwrap()
                    .title,
                owner
            );
            assert!(!catalog.iter().any(|entry| entry.title == alias));
        }
        assert!(registry.references.iter().all(|entry| matches!(
            entry.execution,
            Execution::TelegramReference | Execution::ShellReference
        )));
        for entry in &registry.references {
            assert!(!entry.title.contains("python3"));
            assert!(!entry.title.contains("src/fleet"));
        }
    }
    #[test]
    fn telegram_catalog_only_advertises_native_dispatcher_commands() {
        let catalog = builtin().catalog();
        for entry in catalog.iter().filter(|e| e.category == "Telegram") {
            for command in std::iter::once(&entry.title).chain(entry.aliases.iter()) {
                if let Err(error) = super::super::telegram_commands::parse(command, "fixture_bot") {
                    assert!(!error.contains("Unknown command"), "{command}: {error}");
                }
            }
        }
        for removed in ["/voice", "/mirror", "/console", "/license", "/mode"] {
            assert!(
                !catalog
                    .iter()
                    .any(|e| e.title == removed || e.aliases.iter().any(|a| a == removed))
            );
        }
        assert!(catalog.iter().any(|e| e.title == "/goal"));
        assert!(catalog.iter().any(|e| e.title == "/checkpoint"));
    }
    #[test]
    fn snapshot_shape_preserves_existing_frontend_contract() {
        let registry = builtin();
        let sections =
            serde_json::to_value(registry.section_snapshots(&BTreeMap::new(), true)).unwrap();
        assert_eq!(sections[0]["title"], "Overview");
        assert_eq!(sections[1]["icon"], "workspaces");
        assert_eq!(sections[10]["category"], "Find");
        assert_eq!(
            sections[7]["items"][0]["title"],
            "Hide saved configurations"
        );
        assert_eq!(sections[0]["items"][0]["id"], "setup");
        assert_eq!(
            sections[0]["items"][0]["badges"][0],
            serde_json::json!(["Step by step", "info"])
        );
        let menu = serde_json::to_value(registry.terminal_actions()).unwrap();
        assert_eq!(menu[0]["title"], "Attach and type");
        assert_eq!(menu[0]["short"], "Attach");
    }
}
